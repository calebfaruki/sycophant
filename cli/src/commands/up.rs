use sha2::{Digest, Sha256};

use crate::cli::Values;

use crate::commands::command::{
    self, tenant_token, COMMAND_WRITER_VERBS, OWNER_ANNOTATION, TARGET_ANNOTATION,
};
use crate::commands::common::{step, warn};
use crate::commands::operator::{self, Operator};
use crate::runner::{run_output, run_silent};
use crate::scope::Scope;

/// First 16 hex characters of SHA-256 of the identity.
fn owner_hash(identity: &str) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7]
    )
}

/// `sycoadm tenant up -f <values.yaml>`: render the tenant provision command from
/// the values and apply it. Its installer runs `helm upgrade --install`, so
/// re-running it upgrades the tenant in place.
///
/// The operator logs in before the scope is computed: the login is the owner,
/// and the owner is part of the tenant hash.
pub(crate) fn run(values: &Values) -> Result<(), String> {
    let args = values.helm_args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let manifest = command::render("tenant-provision", &args)?;
    let (name, capacity) = tenant_fields(&manifest)?;
    let op = Operator::login(&Scope::global()?.oidc_dir())?;
    let identity = op.kubeconfig()?.identity;
    let ns = command::resolve_tenant_ns(&command::tenant_namespaces()?, &identity, &name)?;
    let scope = Scope::for_tenant(&ns)?;
    let hash = owner_hash(&identity);
    let owner_arg = format!("tenant.owner={identity}");
    let hash_arg = format!("tenant.ownerHash={hash}");
    let tenant_arg = format!("tenant.hash={}", tenant_token(&ns)?);
    let mut owner_args: Vec<&str> = args.clone();
    owner_args.extend_from_slice(&[
        "--set-string",
        &owner_arg,
        "--set-string",
        &hash_arg,
        "--set-string",
        &tenant_arg,
    ]);
    let manifest = command::render("tenant-provision", &owner_args)?;
    provision(&scope, &op, &manifest, &capacity)
}

/// The chosen name (from `sycophant.md/target`, never from `data.tenant`,
/// which is the hash) and `capacity` a rendered tenant provision command
/// carries.
fn tenant_fields(manifest: &str) -> Result<(String, String), String> {
    let m: serde_json::Value =
        serde_yaml::from_str(manifest).map_err(|e| format!("rendered command is not YAML: {e}"))?;
    let name = command::annotation(&m, TARGET_ANNOTATION).to_string();
    if name.is_empty() {
        return Err(format!(
            "the tenant provision command carries no {TARGET_ANNOTATION}; set tenant.name"
        ));
    }
    let capacity = m["data"]["capacity"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    Ok((name, capacity))
}

/// The tenant provision command that re-provisions the existing tenant in
/// scope at `capacity`. Its chosen name and owner are read back from the
/// Namespace's annotations, so the hash stays the namespace's own.
pub(crate) fn tenant_manifest(scope: &Scope, capacity: &str) -> Result<String, String> {
    let ns = scope.release_name()?;
    let out = run_output("kubectl", &["get", "namespace", &ns, "-o", "json"])?;
    let doc: serde_json::Value =
        serde_json::from_str(&out).map_err(|e| format!("{ns} is not JSON: {e}"))?;
    let name = command::annotation(&doc, TARGET_ANNOTATION);
    if name.is_empty() {
        return Err(format!(
            "{ns} carries no {TARGET_ANNOTATION}; it predates hashed tenant names"
        ));
    }
    let owner = command::annotation(&doc, OWNER_ANNOTATION);
    let hash = owner_hash(owner);
    command::render(
        "tenant-provision",
        &[
            "--set-string",
            &format!("tenant.name={}", name.replace(',', "\\,")),
            "--set-string",
            &format!("tenant.capacity={capacity}"),
            "--set-string",
            &format!("tenant.owner={owner}"),
            "--set-string",
            &format!("tenant.ownerHash={hash}"),
            "--set-string",
            &format!("tenant.hash={}", tenant_token(&ns)?),
        ],
    )
}

/// Apply the tenant provision command for the tenant in scope, then let the
/// operator write commands in its namespace.
pub(crate) fn provision(
    scope: &Scope,
    op: &Operator,
    manifest: &str,
    capacity: &str,
) -> Result<(), String> {
    let ns = scope.release_name()?;

    if run_silent("kubectl", &["get", "namespace", &ns]) {
        warn_capacity_unchanged(&ns, capacity);
    }

    step(&format!("Provisioning tenant {ns}"));
    command::apply(op, "tenant", manifest)?;

    operator::grant_command_writer(&ns, COMMAND_WRITER_VERBS, &op.kubeconfig()?.identity)
}

/// The capacity ResourceQuota is generated once with the namespace; a
/// re-provision does not resize it.
fn warn_capacity_unchanged(ns: &str, capacity: &str) {
    let current = run_output(
        "kubectl",
        &[
            "get",
            "resourcequota",
            &format!("{ns}-capacity"),
            "-n",
            ns,
            "-o",
            "jsonpath={.spec.hard.limits\\.memory}",
        ],
    )
    .unwrap_or_default();
    if !current.is_empty() && !capacity.is_empty() && current != capacity {
        warn(&format!(
            "{ns}: capacity stays {current}; re-provisioning does not resize the quota (asked for {capacity})"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_fields_read_from_the_rendered_command() {
        let f = "kind: ConfigMap\nmetadata:\n  name: provision-tenant-e07ba6258f\n  annotations: {sycophant.md/target: Default}\ndata: {tenant: e07ba6258f, capacity: 2Gi}\n";
        assert_eq!(
            tenant_fields(f).unwrap(),
            ("Default".to_string(), "2Gi".to_string())
        );
        let spaced = "kind: ConfigMap\nmetadata:\n  annotations: {sycophant.md/target: Hello World}\ndata: {tenant: d5f1e46399, capacity: 2Gi}\n";
        assert_eq!(tenant_fields(spaced).unwrap().0, "Hello World");
        assert!(
            tenant_fields("kind: ConfigMap\ndata: {tenant: e07ba6258f, capacity: 2Gi}\n")
                .unwrap_err()
                .contains("carries no sycophant.md/target")
        );
        assert!(tenant_fields(
            "kind: ConfigMap\nmetadata:\n  annotations: {sycophant.md/target: \"\"}\ndata: {capacity: 2Gi}\n"
        )
        .unwrap_err()
        .contains("carries no sycophant.md/target"));
    }

    #[test]
    fn owner_hash_is_first_16_hex_of_sha256() {
        let h = owner_hash(
            "https://sycophant-dex.sycophant-system.svc.cluster.local:5556/dex#operator",
        );
        assert_eq!(h.len(), 16);
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(h, "027164b38d531173");
    }
}
