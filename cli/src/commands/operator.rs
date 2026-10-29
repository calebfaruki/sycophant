//! The operator's IdP identity. `setup` installs the bundled Dex as the issuer
//! the apiserver trusts, with one static password user: the operator. Every
//! provision, reap and suspend command is written under a short-lived Dex ID
//! token obtained with that password, so the cluster sees `<issuer>#<sub>`; a
//! standing cluster credential is refused by the command VAP. The host holds no
//! key and no token.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::commands::command::SYSTEM_NS;
use crate::commands::common::{ok, step, wait_for};
use crate::interrupt;
use crate::runner::{run_output, run_silent, run_stdin, run_stdin_bytes};

// k3s runs kube-apiserver in the node netns with no CoreDNS, so Dex is
// published on a pinned NodePort the apiserver reaches at the node loopback.
// The host reaches the same address through the k3d port mapping.
pub(crate) const DEX_NODEPORT: u16 = 30443;
pub(crate) const ISSUER: &str = "https://127.0.0.1:30443/dex";
// The apiserver audience, and the Dex client the operator logs in through.
pub(crate) const AUDIENCE: &str = "sycophant-cluster";
const DEX_RELEASE: &str = "sycophant-dex";
const DEX_TLS_SECRET: &str = "sycophant-dex-tls";
// Existing clusters already hold the Role and RoleBinding under this name.
const COMMAND_WRITER: &str = "marker-writer";
pub(crate) const EMAIL_ENV: &str = "SYCO_EMAIL";
pub(crate) const PASSWORD_ENV: &str = "SYCO_PASSWORD";

pub(crate) fn auth_config(dir: &Path) -> PathBuf {
    dir.join("auth-config.yaml")
}
pub(crate) fn ca_cert(dir: &Path) -> PathBuf {
    dir.join("ca.crt")
}
fn email_file(dir: &Path) -> PathBuf {
    dir.join("operator-email")
}

/// Generate Dex's self-signed serving cert and render the apiserver
/// AuthenticationConfiguration from the cluster chart, so the mounted file and
/// the chart-rendered ConfigMap agree on issuer, audience, CA, and prefix.
/// Returns the TLS private key, which never touches disk. `values` are the
/// cluster layer's extra values arguments, so a self-service cluster maps its
/// admin group.
pub(crate) fn generate_trust(
    dir: &Path,
    cluster_chart: &Path,
    values: &[String],
) -> Result<String, String> {
    step("Generating Dex issuer trust");
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    let ca = ca_cert(dir).to_string_lossy().into_owned();
    let key = run_output(
        "openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "/dev/stdout",
            "-out",
            &ca,
            "-days",
            "90",
            "-subj",
            "/CN=127.0.0.1",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
            "-addext",
            "basicConstraints=CA:TRUE",
        ],
    )?;
    if !key.contains("PRIVATE KEY") {
        return Err("openssl produced no TLS private key".into());
    }
    let chart = cluster_chart.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec!["template", "sycophant", chart.as_str()];
    args.extend(values.iter().map(String::as_str));
    // The auth file does not depend on the operator, who is not logged in
    // yet, so a placeholder satisfies the prefix check.
    let issuer = format!("idp.issuer={ISSUER}");
    let audience = format!("idp.audience={AUDIENCE}");
    let ca_file = format!("idp.certificateAuthority={ca}");
    let placeholder = format!("selfService.operator={ISSUER}#operator");
    args.extend([
        "--api-versions",
        "policies.kyverno.io/v1/ValidatingPolicy",
        "--set-string",
        issuer.as_str(),
        "--set-string",
        audience.as_str(),
        "--set-file",
        ca_file.as_str(),
        "--set-string",
        placeholder.as_str(),
        "--show-only",
        "templates/apiserver-auth-config.yaml",
    ]);
    let rendered = run_output("helm", &args)?;
    fs::write(auth_config(dir), auth_config_from_render(&rendered)?)
        .map_err(|e| format!("failed to write auth config: {e}"))?;
    ok(&format!("Dex issuer {ISSUER} trust generated"));
    Ok(key)
}

fn auth_config_from_render(rendered: &str) -> Result<String, String> {
    let cm: serde_yaml::Value = serde_yaml::from_str(rendered)
        .map_err(|e| format!("failed to parse rendered auth config: {e}"))?;
    cm.get("data")
        .and_then(|d| d.get("auth-config.yaml"))
        .and_then(serde_yaml::Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "apiserver auth config render is empty".to_string())
}

/// Hand Dex its serving cert and key as a Secret, straight from memory.
pub(crate) fn upload_tls(dir: &Path, key: &str) -> Result<(), String> {
    let crt = fs::read(ca_cert(dir)).map_err(|e| format!("failed to read CA cert: {e}"))?;
    let secret = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "type": "kubernetes.io/tls",
        "metadata": { "name": DEX_TLS_SECRET, "namespace": SYSTEM_NS },
        "data": { "tls.crt": STANDARD.encode(crt), "tls.key": STANDARD.encode(key) },
    })
    .to_string();
    run_stdin("kubectl", &["apply", "-f", "-"], &secret)
}

pub(crate) fn dex_installed() -> bool {
    run_silent("helm", &["status", DEX_RELEASE, "-n", SYSTEM_NS])
}

pub(crate) fn dex_tls_present() -> bool {
    run_silent(
        "kubectl",
        &["get", "secret", DEX_TLS_SECRET, "-n", SYSTEM_NS],
    )
}

/// Install the bundled Dex on the pinned NodePort with one static password
/// user. The bcrypt hash travels to helm on stdin and lands only in the
/// cluster; the password never leaves this process.
pub(crate) fn install_dex(dex_chart: &Path, email: &str, password: &str) -> Result<(), String> {
    step("Installing Dex");
    let chart = dex_chart.to_string_lossy().into_owned();
    let vendored = fs::read_dir(dex_chart.join("charts"))
        .map(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("dex-"))
        })
        .unwrap_or(false);
    if !vendored {
        run_output("helm", &["dependency", "build", &chart])?;
    }
    let line = run_stdin_bytes(
        "htpasswd",
        &["-niB", "-C", "10", email],
        &format!("{password}\n"),
    )?;
    let line = String::from_utf8_lossy(&line);
    let hash = line
        .trim()
        .rsplit_once(':')
        .map(|(_, h)| h.to_string())
        .ok_or("htpasswd produced no hash")?;
    let values = dex_values(email, &hash, &user_id()?).to_string();
    run_stdin(
        "helm",
        &[
            "upgrade",
            "--install",
            DEX_RELEASE,
            &chart,
            "-n",
            SYSTEM_NS,
            "-f",
            "-",
            "--wait",
            "--timeout=5m",
        ],
        &values,
    )?;
    ok(&format!("Dex serving {ISSUER}"));
    Ok(())
}

fn dex_values(email: &str, hash: &str, user_id: &str) -> serde_json::Value {
    serde_json::json!({
        "dex": {
            "service": { "type": "NodePort", "ports": { "https": { "nodePort": DEX_NODEPORT } } },
            "config": {
                "issuer": ISSUER,
                "expiry": { "idTokens": "24h" },
                "oauth2": { "grantTypes": ["password"], "passwordConnector": "local" },
                "staticClients": [{ "id": AUDIENCE, "name": AUDIENCE, "public": true }],
                "staticPasswords": [{
                    "email": email,
                    "hash": hash,
                    "username": "operator",
                    "userID": user_id,
                }],
            },
        },
    })
}

fn random_hex() -> Result<String, String> {
    let mut b = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| format!("failed to read /dev/urandom: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn user_id() -> Result<String, String> {
    let h = random_hex()?;
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

/// `env` when set, else one line read from the terminal, without echo when
/// `hidden`.
pub(crate) fn prompt(label: &str, env: &str, hidden: bool) -> Result<String, String> {
    if let Ok(v) = std::env::var(env) {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    eprint!("{label}: ");
    let _ = io::stderr().flush();
    let tty = hidden && io::stdin().is_terminal();
    if tty {
        interrupt::echo_off();
    }
    let mut line = String::new();
    let read = io::stdin().lock().read_line(&mut line);
    if tty {
        interrupt::echo_on();
        eprintln!();
    }
    read.map_err(|e| format!("failed to read {label}: {e}"))?;
    let v = line.trim_end_matches(['\r', '\n']).to_string();
    if v.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    Ok(v)
}

pub(crate) fn save_email(dir: &Path, email: &str) -> Result<(), String> {
    fs::write(email_file(dir), email).map_err(|e| format!("failed to save operator email: {e}"))
}

/// The existing operator's email and password: from SYCO_EMAIL, the saved
/// email, or a prompt; and from SYCO_PASSWORD or a prompt.
pub(crate) fn credentials(dir: &Path) -> Result<(String, String), String> {
    let email = match (
        std::env::var(EMAIL_ENV),
        fs::read_to_string(email_file(dir)),
    ) {
        (Ok(e), _) if !e.is_empty() => e,
        (_, Ok(e)) if !e.trim().is_empty() => e.trim().to_string(),
        _ => prompt("Operator email", EMAIL_ENV, false)?,
    };
    let password = prompt(&format!("Password for {email}"), PASSWORD_ENV, true)?;
    Ok((email, password))
}

pub(crate) fn command_writer_bound(ns: &str) -> bool {
    run_silent("kubectl", &["get", "rolebinding", COMMAND_WRITER, "-n", ns])
}

/// The operator's login for one command. The password lives only in this
/// process; each `kubeconfig` call trades it for a fresh ID token.
pub(crate) struct Operator {
    issuer: String,
    client: String,
    ca: TempFile,
    email: String,
    password: String,
}

impl Operator {
    /// Prompt for the password and log in against the issuer the installed
    /// cluster release trusts.
    pub(crate) fn login(dir: &Path) -> Result<Self, String> {
        let (email, password) = credentials(dir)?;
        Self::with_password(email, password)
    }

    pub(crate) fn with_password(email: String, password: String) -> Result<Self, String> {
        let out = run_output(
            "helm",
            &[
                "get",
                "values",
                "sycophant",
                "-n",
                SYSTEM_NS,
                "--all",
                "-o",
                "json",
            ],
        )?;
        let v: serde_json::Value = serde_json::from_str(&out)
            .map_err(|e| format!("failed to parse cluster values: {e}"))?;
        let field = |k: &str| {
            v.pointer(&format!("/idp/{k}"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("cluster release carries no idp.{k}"))
        };
        Ok(Operator {
            issuer: field("issuer")?,
            client: field("audience")?,
            ca: TempFile::new("ca.crt", &field("certificateAuthority")?)?,
            email,
            password,
        })
    }

    fn id_token(&self) -> Result<String, String> {
        let out = run_stdin_bytes(
            "curl",
            &[
                "-sS",
                "--fail-with-body",
                "--cacert",
                &self.ca.path(),
                "-u",
                &format!("{}:", self.client),
                "-d",
                "grant_type=password",
                "-d",
                "scope=openid",
                "--data-urlencode",
                &format!("username={}", self.email),
                "--data-urlencode",
                "password@-",
                &format!("{}/token", self.issuer),
            ],
            &self.password,
        )
        .map_err(|e| format!("Dex login failed: {e}"))?;
        let body: serde_json::Value = serde_json::from_slice(&out)
            .map_err(|e| format!("unexpected Dex token response: {e}"))?;
        body.get("id_token")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("Dex returned no id_token: {body}"))
    }

    /// A kubeconfig presenting a fresh ID token, with the identity the
    /// apiserver maps it to.
    pub(crate) fn kubeconfig(&self) -> Result<OperatorKube, String> {
        let token = self.id_token()?;
        let server = run_output(
            "kubectl",
            &[
                "config",
                "view",
                "--minify",
                "-o",
                "jsonpath={.clusters[0].cluster.server}",
            ],
        )?;
        let ca = run_output(
            "kubectl",
            &[
                "config",
                "view",
                "--minify",
                "--raw",
                "-o",
                "jsonpath={.clusters[0].cluster.certificate-authority-data}",
            ],
        )?;
        if ca.is_empty() {
            return Err("could not read the apiserver CA from the current kubeconfig".into());
        }
        // No client cert: kubectl prefers a cert over a token, which would skip OIDC.
        let config = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Config",
            "clusters": [{ "name": "sycophant", "cluster": { "server": server, "certificate-authority-data": ca } }],
            "users": [{ "name": "operator", "user": { "token": token } }],
            "contexts": [{ "name": "operator", "context": { "cluster": "sycophant", "user": "operator" } }],
            "current-context": "operator",
        });
        let file = TempFile::new("kubeconfig", &config.to_string())?;
        let kc = file.path();
        let prefix = format!("{}#", self.issuer);
        let mut identity = String::new();
        // The apiserver's OIDC authenticator initializes lazily on first token use.
        wait_for("operator token authenticated", 60, || {
            identity = run_output(
                "kubectl",
                &[
                    "--kubeconfig",
                    &kc,
                    "auth",
                    "whoami",
                    "-o",
                    "jsonpath={.status.userInfo.username}",
                ],
            )
            .unwrap_or_default();
            identity.starts_with(&prefix)
        })?;
        Ok(OperatorKube { file, identity })
    }
}

/// A 0600 temp file under a random name, created exclusively and removed on
/// drop or interrupt.
pub(crate) struct TempFile(PathBuf);

impl TempFile {
    pub(crate) fn new(suffix: &str, content: &str) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("syco-{}.{suffix}", random_hex()?));
        interrupt::track(path.clone());
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) => {
                // Never ours, so never removed.
                interrupt::forget(&path);
                return Err(format!("failed to create {}: {e}", path.display()));
            }
        };
        let f = TempFile(path);
        file.write_all(content.as_bytes())
            .map_err(|e| format!("failed to write {}: {e}", f.0.display()))?;
        Ok(f)
    }

    pub(crate) fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        interrupt::forget(&self.0);
    }
}

pub(crate) struct OperatorKube {
    file: TempFile,
    pub identity: String,
}

impl OperatorKube {
    pub(crate) fn path(&self) -> String {
        self.file.path()
    }
}

/// Let `identity` write commands in `ns`.
pub(crate) fn grant_command_writer(ns: &str, verbs: &str, identity: &str) -> Result<(), String> {
    let role = run_output(
        "kubectl",
        &[
            "create",
            "role",
            COMMAND_WRITER,
            "-n",
            ns,
            &format!("--verb={verbs}"),
            "--resource=configmaps",
            "--dry-run=client",
            "-o",
            "yaml",
        ],
    )?;
    run_stdin("kubectl", &["apply", "-f", "-"], &role)?;
    let binding = run_output(
        "kubectl",
        &[
            "create",
            "rolebinding",
            COMMAND_WRITER,
            "-n",
            ns,
            &format!("--role={COMMAND_WRITER}"),
            &format!("--user={identity}"),
            "--dry-run=client",
            "-o",
            "yaml",
        ],
    )?;
    run_stdin("kubectl", &["apply", "-f", "-"], &binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn auth_config_extracted_from_rendered_configmap() {
        let rendered = "apiVersion: v1\nkind: ConfigMap\ndata:\n  auth-config.yaml: |\n    kind: AuthenticationConfiguration\n";
        assert_eq!(
            auth_config_from_render(rendered).unwrap(),
            "kind: AuthenticationConfiguration\n"
        );
        assert!(auth_config_from_render("data: {}").is_err());
    }

    #[test]
    fn dex_values_enable_password_grant_for_one_user() {
        let v = dex_values("op@example.com", "$2y$10$h", "u-1");
        let c = &v["dex"]["config"];
        assert_eq!(c["issuer"], ISSUER);
        assert_eq!(c["oauth2"]["grantTypes"], serde_json::json!(["password"]));
        assert_eq!(c["oauth2"]["passwordConnector"], "local");
        assert_eq!(c["staticClients"][0]["id"], AUDIENCE);
        assert_eq!(c["staticClients"][0]["public"], true);
        assert_eq!(c["staticPasswords"][0]["email"], "op@example.com");
        assert_eq!(c["staticPasswords"][0]["hash"], "$2y$10$h");
        assert_eq!(c["staticPasswords"][0]["userID"], "u-1");
        assert_eq!(
            v["dex"]["service"]["ports"]["https"]["nodePort"],
            DEX_NODEPORT
        );
    }

    #[test]
    fn user_id_is_uuid_shaped() {
        let id = user_id().unwrap();
        let parts: Vec<_> = id.split('-').map(str::len).collect();
        assert_eq!(parts, [8, 4, 4, 4, 12]);
        assert_ne!(id, user_id().unwrap());
    }

    #[test]
    fn temp_file_is_private_and_removed() {
        let f = TempFile::new("t", "x").unwrap();
        let p = f.0.clone();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_ne!(TempFile::new("t", "x").unwrap().0, p);
        drop(f);
        assert!(!p.exists());
    }
}
