//! `syco tenant kernel set/list/delete` — content-tier Kernel CR management.
//!
//! A Kernel binds per-workspace (`metadata.name == workspace`) and selects where
//! a workspace's persona content (AGENTS.md, agents/*.md, skills/*.md) comes
//! from: the host directory (local live-edit dev) or an S3 bucket. `set` authors
//! the CR via `kubectl apply`. HostPath content is delivered by the chart's
//! static `/etc/kernels/<namespace>` mount; content lives at the convention path
//! `<hostPathBase>/<namespace>/<workspace>`, or a custom directory when `--path`
//! overrides it. S3 is delivered by the mainframe-controller's one-shot sync Job.

use serde::Serialize;

use crate::cli::{KernelCmd, KernelList, KernelSet, KernelSub};
use crate::commands::common;
use crate::runner::{run_output, run_stdin};
use crate::scope::Scope;

pub(crate) fn run(scope: &Scope, cmd: KernelCmd) -> Result<(), String> {
    match cmd.sub {
        KernelSub::Set(set) => do_set(scope, set),
        KernelSub::List(list) => do_list(scope, list),
        KernelSub::Delete(del) => do_delete(scope, &del.workspace),
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KernelEntry {
    pub workspace: String,
    pub kind: String,
}

/// Parsed, validated kernel source. `parse_kernel_source` enforces the
/// kind↔flags XOR up front so `build_kernel_cr` stays total.
#[derive(Debug, PartialEq)]
enum KernelSource {
    // HostPath delivers at the convention path <kernels-root>/<namespace>/
    // <workspace> via the chart's static mount. `path` is an OPTIONAL override
    // of the host source directory (absolute); `None` → convention default.
    HostPath {
        path: Option<String>,
    },
    S3 {
        endpoint: String,
        bucket: String,
        prefix: String,
        region: String,
        force_path_style: bool,
        credentials: String,
        access_key_id_key: Option<String>,
        secret_access_key_key: Option<String>,
    },
}

/// Validate `--kind` against the variant flags: hostpath takes only `--path`
/// (an optional absolute override; absent → convention default); s3 requires
/// endpoint/bucket/prefix/credentials. Region defaults to us-east-1 and
/// forcePathStyle to true (matching the chart's prior defaults).
fn parse_kernel_source(cmd: &KernelSet) -> Result<KernelSource, String> {
    match cmd.kind.as_str() {
        "hostpath" => {
            let s3_flag_present = cmd.endpoint.is_some()
                || cmd.bucket.is_some()
                || cmd.prefix.is_some()
                || cmd.region.is_some()
                || cmd.force_path_style.is_some()
                || cmd.credentials.is_some()
                || cmd.access_key_id_key.is_some()
                || cmd.secret_access_key_key.is_some();
            if s3_flag_present {
                return Err("--kind hostpath takes only --path; S3 flags are not allowed.".into());
            }
            // `--path` is an optional override of the host source dir. When
            // given it must be absolute (mirrors the CRD `^/.+` schema); absent
            // → convention default <hostPathBase>/<namespace>/<workspace>.
            if let Some(p) = &cmd.path {
                if !p.starts_with('/') {
                    return Err(format!(
                        "--path must be an absolute path (start with '/'), got {p:?}."
                    ));
                }
            }
            Ok(KernelSource::HostPath {
                path: cmd.path.clone(),
            })
        }
        "s3" => {
            let endpoint = cmd
                .endpoint
                .clone()
                .ok_or_else(|| "--endpoint is required for --kind s3.".to_string())?;
            let bucket = cmd
                .bucket
                .clone()
                .ok_or_else(|| "--bucket is required for --kind s3.".to_string())?;
            let prefix = cmd
                .prefix
                .clone()
                .ok_or_else(|| "--prefix is required for --kind s3.".to_string())?;
            let credentials = cmd
                .credentials
                .clone()
                .ok_or_else(|| "--credentials <secret> is required for --kind s3.".to_string())?;
            Ok(KernelSource::S3 {
                endpoint,
                bucket,
                prefix,
                region: cmd
                    .region
                    .clone()
                    .unwrap_or_else(|| "us-east-1".to_string()),
                force_path_style: cmd.force_path_style.unwrap_or(true),
                credentials,
                access_key_id_key: cmd.access_key_id_key.clone(),
                secret_access_key_key: cmd.secret_access_key_key.clone(),
            })
        }
        other => Err(format!(
            "unknown --kind {other:?} (expected hostpath or s3)."
        )),
    }
}

/// Build a `Kernel` CR for `kubectl apply`. `workspace` is the metadata name (the
/// per-workspace binding). Spec-only — `status` is controller-owned.
fn build_kernel_cr(workspace: &str, namespace: &str, source: &KernelSource) -> String {
    let name_q = serde_json::to_string(workspace).unwrap_or_default();
    let ns_q = serde_json::to_string(namespace).unwrap_or_default();
    let mut out = format!(
        r#"apiVersion: sycophant.md/v1
kind: Kernel
metadata:
  name: {name_q}
  namespace: {ns_q}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: kernel
spec:
"#
    );
    match source {
        KernelSource::HostPath { path } => {
            out.push_str("  kind: HostPath\n");
            if let Some(p) = path {
                let p_q = serde_json::to_string(p).unwrap_or_default();
                out.push_str(&format!("  hostPath:\n    path: {p_q}\n"));
            }
        }
        KernelSource::S3 {
            endpoint,
            bucket,
            prefix,
            region,
            force_path_style,
            credentials,
            access_key_id_key,
            secret_access_key_key,
        } => {
            let endpoint_q = serde_json::to_string(endpoint).unwrap_or_default();
            let bucket_q = serde_json::to_string(bucket).unwrap_or_default();
            let prefix_q = serde_json::to_string(prefix).unwrap_or_default();
            let region_q = serde_json::to_string(region).unwrap_or_default();
            let creds_q = serde_json::to_string(credentials).unwrap_or_default();
            out.push_str(&format!(
                "  kind: S3\n  s3:\n    endpoint: {endpoint_q}\n    bucket: {bucket_q}\n    \
                 prefix: {prefix_q}\n    region: {region_q}\n    \
                 forcePathStyle: {force_path_style}\n    credentials:\n      name: {creds_q}\n"
            ));
            if let Some(k) = access_key_id_key {
                let k_q = serde_json::to_string(k).unwrap_or_default();
                out.push_str(&format!("      accessKeyIdKey: {k_q}\n"));
            }
            if let Some(k) = secret_access_key_key {
                let k_q = serde_json::to_string(k).unwrap_or_default();
                out.push_str(&format!("      secretAccessKeyKey: {k_q}\n"));
            }
        }
    }
    out
}

/// Refuse to author a Kernel for a workspace the tenant values don't declare.
/// Tolerant of a missing/unreadable values file (GitOps operators declare
/// workspaces outside the CLI scaffold, so the check can't be load-bearing).
fn ensure_workspace_declared(scope: &Scope, workspace: &str) -> Result<(), String> {
    let Ok(root) = crate::values::load(&scope.values_file()) else {
        return Ok(());
    };
    let workspaces = root.get("workspaces").and_then(|v| v.as_mapping());
    crate::commands::workspace::workspace_show_data(workspaces, workspace)
        .map(|_| ())
        .map_err(|_| {
            format!(
                "Workspace \"{workspace}\" is not declared. \
                 Run: syco tenant workspace create {workspace} --ns <name>"
            )
        })
}

fn do_set(scope: &Scope, cmd: KernelSet) -> Result<(), String> {
    let source = parse_kernel_source(&cmd)?;
    ensure_workspace_declared(scope, &cmd.workspace)?;
    let namespace = scope.release_name()?;

    let yaml = build_kernel_cr(&cmd.workspace, &namespace, &source);
    run_stdin("kubectl", &["apply", "-n", &namespace, "-f", "-"], &yaml)?;

    eprintln!(
        "Kernel for workspace '{}' configured ({}).",
        cmd.workspace, cmd.kind
    );
    eprintln!("Run `syco tenant up --ns {namespace}` to deliver it.");
    Ok(())
}

fn do_list(scope: &Scope, cmd: KernelList) -> Result<(), String> {
    let namespace = scope.release_name()?;
    let output = run_output(
        "kubectl",
        &[
            "get",
            "kernels.sycophant.md",
            "-n",
            &namespace,
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\"\\t\"}{.spec.kind}{\"\\n\"}{end}",
        ],
    )?;
    let entries = parse_kernel_list(&output);

    if cmd.json {
        let json =
            serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
        return Ok(());
    }

    if entries.is_empty() {
        eprintln!("No kernels configured.");
        return Ok(());
    }

    eprintln!("{:<40} KIND", "WORKSPACE");
    for e in &entries {
        eprintln!("{:<40} {}", e.workspace, e.kind);
    }
    Ok(())
}

/// Parse the tab-separated `kubectl get kernels` jsonpath output into entries.
pub(crate) fn parse_kernel_list(kubectl_output: &str) -> Vec<KernelEntry> {
    common::parse_tab_rows(kubectl_output)
        .iter()
        .map(|c| KernelEntry {
            workspace: common::col(c, 0),
            kind: common::col(c, 1),
        })
        .collect()
}

fn do_delete(scope: &Scope, workspace: &str) -> Result<(), String> {
    let namespace = scope.release_name()?;
    if common::delete_cr("kernel.sycophant.md", workspace, &namespace)? {
        eprintln!("Kernel for workspace '{workspace}' deleted.");
    } else {
        eprintln!("Kernel for workspace '{workspace}' not found.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> serde_yaml::Value {
        serde_yaml::from_str(yaml).expect("builder output must be valid YAML")
    }

    /// A KernelSet with everything unset — tests set only the fields they need.
    fn base_set(workspace: &str, kind: &str) -> KernelSet {
        KernelSet {
            workspace: workspace.into(),
            kind: kind.into(),
            path: None,
            endpoint: None,
            bucket: None,
            prefix: None,
            region: None,
            force_path_style: None,
            credentials: None,
            access_key_id_key: None,
            secret_access_key_key: None,
        }
    }

    // -- parse_kernel_source validation --

    #[test]
    fn hostpath_parses_without_path() {
        // Absent --path → convention default (no override).
        let c = base_set("ws", "hostpath");
        assert_eq!(
            parse_kernel_source(&c).unwrap(),
            KernelSource::HostPath { path: None }
        );
    }

    #[test]
    fn hostpath_with_absolute_path_is_the_override() {
        let mut c = base_set("ws", "hostpath");
        c.path = Some("/Users/me/personas/web".into());
        assert_eq!(
            parse_kernel_source(&c).unwrap(),
            KernelSource::HostPath {
                path: Some("/Users/me/personas/web".into())
            }
        );
    }

    #[test]
    fn hostpath_rejects_relative_path() {
        // Mutant dropping the absolute check would author a relative override the
        // CRD rejects at admission (schema pattern ^/.+) — fail early instead.
        let mut c = base_set("ws", "hostpath");
        c.path = Some("relative/dir".into());
        assert!(
            parse_kernel_source(&c).unwrap_err().contains("absolute"),
            "relative --path must be rejected"
        );
    }

    #[test]
    fn hostpath_rejects_s3_flags() {
        // Mutant dropping the XOR guard would let an S3 flag ride on a HostPath
        // kernel and silently produce an invalid mixed spec.
        let mut c = base_set("ws", "hostpath");
        c.bucket = Some("b".into());
        let err = parse_kernel_source(&c).unwrap_err();
        assert!(err.contains("only --path"), "got: {err}");
    }

    #[test]
    fn s3_requires_endpoint_bucket_prefix_credentials() {
        let mut c = base_set("ws", "s3");
        assert!(parse_kernel_source(&c).is_err());
        c.endpoint = Some("http://gw:7070".into());
        assert!(parse_kernel_source(&c).unwrap_err().contains("--bucket"));
        c.bucket = Some("bkt".into());
        assert!(parse_kernel_source(&c).unwrap_err().contains("--prefix"));
        c.prefix = Some("t/".into());
        assert!(parse_kernel_source(&c)
            .unwrap_err()
            .contains("--credentials"));
        c.credentials = Some("s3-creds".into());
        assert!(parse_kernel_source(&c).is_ok());
    }

    #[test]
    fn s3_defaults_region_and_force_path_style() {
        // Mutant flipping the forcePathStyle default to false breaks self-hosted
        // gateways (Versitygw/MinIO) whose virtual-host URLs need path-style.
        let mut c = base_set("ws", "s3");
        c.endpoint = Some("http://gw".into());
        c.bucket = Some("b".into());
        c.prefix = Some("p/".into());
        c.credentials = Some("s".into());
        match parse_kernel_source(&c).unwrap() {
            KernelSource::S3 {
                region,
                force_path_style,
                ..
            } => {
                assert_eq!(region, "us-east-1");
                assert!(force_path_style);
            }
            _ => panic!("expected S3"),
        }
    }

    #[test]
    fn s3_force_path_style_override_wins() {
        let mut c = base_set("ws", "s3");
        c.endpoint = Some("http://gw".into());
        c.bucket = Some("b".into());
        c.prefix = Some("p/".into());
        c.credentials = Some("s".into());
        c.force_path_style = Some(false);
        match parse_kernel_source(&c).unwrap() {
            KernelSource::S3 {
                force_path_style, ..
            } => assert!(!force_path_style),
            _ => panic!("expected S3"),
        }
    }

    #[test]
    fn unknown_kind_errors() {
        let c = base_set("ws", "nfs");
        assert!(parse_kernel_source(&c).unwrap_err().contains("nfs"));
    }

    // -- build_kernel_cr shape --

    #[test]
    fn hostpath_cr_shape_no_override() {
        let v = parse(&build_kernel_cr(
            "ws",
            "dev",
            &KernelSource::HostPath { path: None },
        ));
        assert_eq!(v["kind"].as_str(), Some("Kernel"));
        assert_eq!(v["metadata"]["name"].as_str(), Some("ws"));
        assert_eq!(v["metadata"]["namespace"].as_str(), Some("dev"));
        assert_eq!(v["spec"]["kind"].as_str(), Some("HostPath"));
        // No override → no hostPath block (convention default).
        assert!(v["spec"].get("hostPath").is_none());
        assert!(v["spec"].get("s3").is_none());
        assert!(v.get("status").is_none(), "builder is spec-only");
    }

    #[test]
    fn hostpath_cr_shape_with_override() {
        // Mutant dropping the hostPath block would silently lose the operator's
        // --path override → the kernel would serve from the wrong directory.
        let v = parse(&build_kernel_cr(
            "ws",
            "dev",
            &KernelSource::HostPath {
                path: Some("/custom/dir".into()),
            },
        ));
        assert_eq!(v["spec"]["kind"].as_str(), Some("HostPath"));
        assert_eq!(v["spec"]["hostPath"]["path"].as_str(), Some("/custom/dir"));
    }

    #[test]
    fn s3_cr_shape_full() {
        let v = parse(&build_kernel_cr(
            "ws",
            "dev",
            &KernelSource::S3 {
                endpoint: "http://versitygw:7070".into(),
                bucket: "tenants".into(),
                prefix: "abc/mainframe/".into(),
                region: "eu-west-1".into(),
                force_path_style: true,
                credentials: "s3-creds".into(),
                access_key_id_key: Some("id".into()),
                secret_access_key_key: Some("secret".into()),
            },
        ));
        assert_eq!(v["spec"]["kind"].as_str(), Some("S3"));
        assert_eq!(
            v["spec"]["s3"]["endpoint"].as_str(),
            Some("http://versitygw:7070")
        );
        assert_eq!(v["spec"]["s3"]["bucket"].as_str(), Some("tenants"));
        assert_eq!(v["spec"]["s3"]["prefix"].as_str(), Some("abc/mainframe/"));
        assert_eq!(v["spec"]["s3"]["region"].as_str(), Some("eu-west-1"));
        assert_eq!(v["spec"]["s3"]["forcePathStyle"].as_bool(), Some(true));
        assert_eq!(
            v["spec"]["s3"]["credentials"]["name"].as_str(),
            Some("s3-creds")
        );
        assert_eq!(
            v["spec"]["s3"]["credentials"]["accessKeyIdKey"].as_str(),
            Some("id")
        );
        assert_eq!(
            v["spec"]["s3"]["credentials"]["secretAccessKeyKey"].as_str(),
            Some("secret")
        );
    }

    #[test]
    fn s3_cr_omits_optional_credential_keys_when_unset() {
        // Unset key overrides must not render null/empty keys — the controller
        // defaults them (access-key-id / secret-access-key).
        let v = parse(&build_kernel_cr(
            "ws",
            "dev",
            &KernelSource::S3 {
                endpoint: "http://gw".into(),
                bucket: "b".into(),
                prefix: "p/".into(),
                region: "us-east-1".into(),
                force_path_style: true,
                credentials: "creds".into(),
                access_key_id_key: None,
                secret_access_key_key: None,
            },
        ));
        let creds = &v["spec"]["s3"]["credentials"];
        assert_eq!(creds["name"].as_str(), Some("creds"));
        assert!(creds.get("accessKeyIdKey").is_none());
        assert!(creds.get("secretAccessKeyKey").is_none());
    }

    #[test]
    fn cr_has_operator_labels_not_helm() {
        let v = parse(&build_kernel_cr(
            "ws",
            "dev",
            &KernelSource::HostPath { path: None },
        ));
        assert_eq!(
            v["metadata"]["labels"]["sycophant.md/type"].as_str(),
            Some("kernel")
        );
        assert!(v["metadata"]["labels"]["app.kubernetes.io/managed-by"].is_null());
    }

    // -- parse_kernel_list --

    #[test]
    fn parse_kernel_list_empty_input() {
        assert_eq!(parse_kernel_list(""), Vec::new());
        assert_eq!(parse_kernel_list("  \n \n"), Vec::new());
    }

    #[test]
    fn parse_kernel_list_splits_tab_columns() {
        let entries = parse_kernel_list("alpha\tS3\nbeta\tHostPath\n");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].workspace, "alpha");
        assert_eq!(entries[0].kind, "S3");
        assert_eq!(entries[1].workspace, "beta");
        assert_eq!(entries[1].kind, "HostPath");
    }

    #[test]
    fn kernel_entry_serializes_to_camel_case_json() {
        let entry = KernelEntry {
            workspace: "alpha".into(),
            kind: "S3".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"workspace\":\"alpha\""));
        assert!(json.contains("\"kind\":\"S3\""));
    }
}
