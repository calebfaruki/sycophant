use serde::Serialize;

use crate::cli::{ClientCmd, ClientList, ClientSet, ClientSub};
use crate::commands::common;
use crate::runner::{run_output, run_stdin};
use crate::scope::Scope;

pub(crate) fn run(scope: &Scope, cmd: ClientCmd) -> Result<(), String> {
    match cmd.sub {
        ClientSub::Set(set) => do_set(scope, set),
        ClientSub::List(list) => do_list(scope, list),
        ClientSub::Delete(del) => do_delete(scope, &del.name),
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientEntry {
    pub name: String,
    pub workspaces: String,
}

/// Build a `Client` CR for `kubectl apply`. `name` is the metadata name (the
/// device identity / signature kid). `workspaces` is the authorized list gated
/// against the per-request workspace assertion at verify time. Spec-only:
/// `status` (enrollmentCode, publicKey, enrolledAt) is owned by the controller
/// on the status subresource and is never written here, so a plain apply never
/// disturbs an enrolled device's key.
fn build_client_cr(name: &str, namespace: &str, workspaces: &[String]) -> String {
    let name_q = serde_json::to_string(name).unwrap_or_default();
    let ns_q = serde_json::to_string(namespace).unwrap_or_default();
    let mut out = format!(
        r#"apiVersion: sycophant.md/v1
kind: Client
metadata:
  name: {name_q}
  namespace: {ns_q}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: client
spec:
  workspaces:"#
    );
    if workspaces.is_empty() {
        out.push_str(" []\n");
    } else {
        out.push('\n');
        for ws in workspaces {
            let ws_q = serde_json::to_string(ws).unwrap_or_default();
            out.push_str(&format!("    - {ws_q}\n"));
        }
    }
    out
}

fn do_set(scope: &Scope, cmd: ClientSet) -> Result<(), String> {
    if cmd.workspace.is_empty() {
        return Err("--workspace <name> is required at least once (a client must be \
                    authorized for ≥1 workspace)."
            .to_string());
    }
    let namespace = scope.release_name()?;
    common::ensure_namespace(&namespace);

    let yaml = build_client_cr(&cmd.name, &namespace, &cmd.workspace);
    run_stdin("kubectl", &["apply", "-n", &namespace, "-f", "-"], &yaml)?;

    eprintln!(
        "Client '{}' configured ({} workspace(s)).",
        cmd.name,
        cmd.workspace.len()
    );
    Ok(())
}

fn do_list(scope: &Scope, cmd: ClientList) -> Result<(), String> {
    let namespace = scope.release_name()?;
    let output = run_output(
        "kubectl",
        &[
            "get",
            "clients.sycophant.md",
            "-n",
            &namespace,
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\"\\t\"}{.spec.workspaces}{\"\\n\"}{end}",
        ],
    )?;
    let entries = parse_client_list(&output);

    if cmd.json {
        let json =
            serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
        return Ok(());
    }

    if entries.is_empty() {
        eprintln!("No clients configured.");
        return Ok(());
    }

    eprintln!("{:<40} WORKSPACES", "NAME");
    for e in &entries {
        eprintln!("{:<40} {}", e.name, e.workspaces);
    }
    Ok(())
}

/// Parse the tab-separated `kubectl get clients` jsonpath output into entries.
/// The `workspaces` column is the raw bracketed array string (e.g. `[a b]`) —
/// kept verbatim for display, not parsed.
pub(crate) fn parse_client_list(kubectl_output: &str) -> Vec<ClientEntry> {
    kubectl_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let name = cols.next()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(ClientEntry {
                name,
                workspaces: cols.next().unwrap_or_default().trim().to_string(),
            })
        })
        .collect()
}

fn do_delete(scope: &Scope, name: &str) -> Result<(), String> {
    let namespace = scope.release_name()?;
    if common::delete_cr("client.sycophant.md", name, &namespace)? {
        eprintln!("Client '{name}' deleted.");
    } else {
        eprintln!("Client '{name}' not found.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> serde_yaml::Value {
        serde_yaml::from_str(yaml).expect("builder output must be valid YAML")
    }

    #[test]
    fn client_cr_has_kind_and_workspaces_sequence() {
        let v = parse(&build_client_cr(
            "klein-wenner-flutter",
            "dev",
            &["alpha".into(), "beta".into()],
        ));
        assert_eq!(v["kind"].as_str(), Some("Client"));
        assert_eq!(v["metadata"]["name"].as_str(), Some("klein-wenner-flutter"));
        assert_eq!(v["metadata"]["namespace"].as_str(), Some("dev"));
        let ws = v["spec"]["workspaces"]
            .as_sequence()
            .expect("workspaces must be a sequence");
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].as_str(), Some("alpha"));
        assert_eq!(ws[1].as_str(), Some("beta"));
    }

    #[test]
    fn client_cr_workspaces_is_sequence_not_scalar() {
        // Mutation guard: spec.workspaces is a YAML list, never a scalar string.
        let v = parse(&build_client_cr("c", "dev", &["only".into()]));
        assert!(v["spec"]["workspaces"].as_sequence().is_some());
        assert!(v["spec"]["workspaces"].as_str().is_none());
    }

    #[test]
    fn client_cr_empty_workspaces_is_empty_sequence() {
        // The builder must still emit a valid, round-trippable empty sequence
        // (do_set guards against this case, but the builder stays total).
        let v = parse(&build_client_cr("c", "dev", &[]));
        let ws = v["spec"]["workspaces"]
            .as_sequence()
            .expect("empty workspaces must be a sequence, not null");
        assert!(ws.is_empty());
    }

    #[test]
    fn client_cr_has_operator_labels_not_helm() {
        let v = parse(&build_client_cr("c", "dev", &["a".into()]));
        assert_eq!(
            v["metadata"]["labels"]["app.kubernetes.io/part-of"].as_str(),
            Some("sycophant")
        );
        assert_eq!(
            v["metadata"]["labels"]["sycophant.md/type"].as_str(),
            Some("client")
        );
        assert!(v["metadata"]["labels"]["app.kubernetes.io/managed-by"].is_null());
    }

    #[test]
    fn client_cr_has_no_status_block() {
        // The builder is spec-only; emitting a status stanza could let a plain
        // apply clobber the controller-owned enrollment (publicKey).
        let yaml = build_client_cr("c", "dev", &["a".into()]);
        assert!(parse(&yaml).get("status").is_none());
        assert!(!yaml.contains("publicKey"));
        assert!(!yaml.contains("status"));
    }

    #[test]
    fn client_cr_workspace_value_is_quoted() {
        // A workspace containing YAML-special chars must round-trip exactly.
        let v = parse(&build_client_cr("c", "dev", &["a: b".into()]));
        assert_eq!(v["spec"]["workspaces"][0].as_str(), Some("a: b"));
    }

    #[test]
    fn parse_client_list_empty_input() {
        assert_eq!(parse_client_list(""), Vec::new());
        assert_eq!(parse_client_list("  \n \n"), Vec::new());
    }

    #[test]
    fn parse_client_list_splits_tab_columns() {
        let entries = parse_client_list("klein-wenner-flutter\t[alpha beta]\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "klein-wenner-flutter");
        assert_eq!(entries[0].workspaces, "[alpha beta]");
    }

    #[test]
    fn client_entry_serializes_to_camel_case_json() {
        let entry = ClientEntry {
            name: "dev-laptop".into(),
            workspaces: "[alpha]".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"name\":\"dev-laptop\""));
        assert!(json.contains("\"workspaces\":\"[alpha]\""));
    }
}
