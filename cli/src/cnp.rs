//! llm-job egress CiliumNetworkPolicy authoring, from OUTSIDE the tenant.
//!
//! The `llm-job-egress` CNP is a tenant-wide UNION of all Provider hosts (every
//! llm-job pod shares one policy). It is recomputed from the live Provider CR set
//! and applied by `syco` (operator kubeconfig = external caller), so the in-tenant
//! CNP-immutability invariant stays absolute. It composes additively on top of the
//! chart's `llm-job-baseline` fail-closed floor — both carry `rules.dns` on :53
//! (never L4-only) so the L7 DNS allowlists union rather than shadow.

use crate::runner::{run_output, run_stdin};

/// Canonical base URL for a provider format when the Provider CR omits `baseUrl`.
/// Ported verbatim from `hangar-controller::job::canonical_base_url` so the
/// CLI-authored union matches what the controller would have produced.
pub(crate) fn canonical_base_url(format: &str) -> String {
    match format {
        "anthropic" => "https://api.anthropic.com/v1".into(),
        "openai" => "https://api.openai.com/v1".into(),
        "gemini" => "https://generativelanguage.googleapis.com".into(),
        _ => String::new(),
    }
}

/// Extract the bare DNS host from a base URL (strip scheme, path, and port).
/// Returns `None` for an empty/host-less URL.
pub(crate) fn provider_host(base_url: &str) -> Option<String> {
    let no_scheme = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))
        .unwrap_or(base_url);
    let host = no_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// A provider's reachable endpoint: bare host, port (parsed from the base URL,
/// scheme default otherwise), and whether the host is a private/loopback IPv4
/// literal. Private endpoints are pinned by `toCIDR` on their port; public hosts
/// take the DNS-allowlist + `toFQDNs:443` path.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProviderEndpoint {
    pub host: String,
    pub port: u16,
    pub is_private: bool,
}

/// Parse the port from a base URL. An explicit `:port` wins; otherwise the scheme
/// default (443 for https or scheme-less, 80 for http).
pub(crate) fn provider_port(base_url: &str) -> u16 {
    let (is_http, rest) = match base_url.strip_prefix("https://") {
        Some(r) => (false, r),
        None => match base_url.strip_prefix("http://") {
            Some(r) => (true, r),
            None => (false, base_url),
        },
    };
    let authority = rest.split('/').next().unwrap_or("");
    if let Some((_, port_str)) = authority.rsplit_once(':') {
        if let Ok(port) = port_str.parse::<u16>() {
            return port;
        }
    }
    if is_http {
        80
    } else {
        443
    }
}

/// True if `host` is a literal private/loopback/link-local IPv4 address. DNS names
/// and IPv6 return false — they take the toFQDNs path, not the toCIDR pin.
fn is_private_or_loopback_ip(host: &str) -> bool {
    host.parse::<std::net::Ipv4Addr>()
        .map(|ip| ip.is_private() || ip.is_loopback() || ip.is_link_local())
        .unwrap_or(false)
}

/// Compute the deduplicated endpoint union across providers. Each endpoint's host
/// comes from its `baseUrl` (falling back to the canonical URL for its format),
/// with the port parsed from the URL and a private/loopback-IP classification.
/// Order-preserving; dedup on host. A provider with no resolvable host contributes
/// nothing (its llm-jobs stay fail-closed under the baseline — safe, not open).
pub(crate) fn endpoint_union(providers: &[(String, Option<String>)]) -> Vec<ProviderEndpoint> {
    let mut endpoints: Vec<ProviderEndpoint> = Vec::new();
    for (format, base_url) in providers {
        let url = match base_url {
            Some(u) if !u.is_empty() => u.clone(),
            _ => canonical_base_url(format),
        };
        if let Some(host) = provider_host(&url) {
            if endpoints.iter().any(|e| e.host == host) {
                continue;
            }
            let port = provider_port(&url);
            let is_private = is_private_or_loopback_ip(&host);
            endpoints.push(ProviderEndpoint {
                host,
                port,
                is_private,
            });
        }
    }
    endpoints
}

/// Parse the tab-separated `kubectl get providers` jsonpath output (`format\tbaseUrl`)
/// into `(format, Option<baseUrl>)` pairs. Absent baseUrl -> None.
pub(crate) fn parse_provider_list(kubectl_output: &str) -> Vec<(String, Option<String>)> {
    kubectl_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let format = cols.next()?.trim().to_string();
            if format.is_empty() {
                return None;
            }
            let base_url = cols
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            Some((format, base_url))
        })
        .collect()
}

/// Build the `llm-job-egress` union CNP YAML for `kubectl apply`. kube-dns:53 with
/// an L7 `rules.dns` allowlist (hangar-ctrl FQDN + each PUBLIC provider host),
/// hangar-ctrl:9090, `toFQDNs:443` for the public host union (each public host
/// in BOTH `rules.dns` and `toFQDNs` so Cilium's DNS proxy learns the FQDN->IP
/// mapping the L4 toFQDNs rule needs), and a `toCIDR <ip>/32` rule on the parsed
/// port for each PRIVATE endpoint (a private IP is reached directly — no DNS, no
/// toFQDNs). NO catch-all. Ports are quoted strings (Cilium).
pub(crate) fn build_llm_egress_cnp_yaml(namespace: &str, endpoints: &[ProviderEndpoint]) -> String {
    let ns_q = serde_json::to_string(namespace).unwrap_or_default();
    let hangar_fqdn_q =
        serde_json::to_string(&format!("hangar-ctrl.{namespace}.svc.cluster.local"))
            .unwrap_or_default();
    let public: Vec<&ProviderEndpoint> = endpoints.iter().filter(|e| !e.is_private).collect();
    let private: Vec<&ProviderEndpoint> = endpoints.iter().filter(|e| e.is_private).collect();
    let mut out = format!(
        r#"apiVersion: cilium.io/v2
kind: CiliumNetworkPolicy
metadata:
  name: llm-job-egress
  namespace: {ns_q}
  labels:
    app.kubernetes.io/part-of: sycophant
spec:
  endpointSelector:
    matchLabels:
      app.kubernetes.io/component: llm-job
  egress:
    - toEndpoints:
        - matchLabels:
            io.kubernetes.pod.namespace: kube-system
            k8s-app: kube-dns
      toPorts:
        - ports:
            - port: "53"
              protocol: UDP
            - port: "53"
              protocol: TCP
          rules:
            dns:
              - matchName: {hangar_fqdn_q}
"#
    );
    for ep in &public {
        let host_q = serde_json::to_string(&ep.host).unwrap_or_default();
        out.push_str(&format!("              - matchName: {host_q}\n"));
    }
    out.push_str(
        r#"    - toEndpoints:
        - matchLabels:
            app.kubernetes.io/name: hangar-ctrl
      toPorts:
        - ports:
            - port: "9090"
              protocol: TCP
"#,
    );
    if !public.is_empty() {
        out.push_str("    - toFQDNs:\n");
        for ep in &public {
            let host_q = serde_json::to_string(&ep.host).unwrap_or_default();
            out.push_str(&format!("        - matchName: {host_q}\n"));
        }
        out.push_str(
            "      toPorts:\n        - ports:\n            - port: \"443\"\n              protocol: TCP\n",
        );
    }
    for ep in &private {
        let cidr_q = serde_json::to_string(&format!("{}/32", ep.host)).unwrap_or_default();
        out.push_str("    - toCIDR:\n");
        out.push_str(&format!("        - {cidr_q}\n"));
        out.push_str(&format!(
            "      toPorts:\n        - ports:\n            - port: \"{}\"\n              protocol: TCP\n",
            ep.port
        ));
    }
    out
}

/// Recompute the `llm-job-egress` union from the live Provider set and apply it
/// (or delete it when there are no providers — the chart baseline is then the sole
/// fail-closed floor). Called by `syco provider`/`syco model` after a Provider CR
/// change so the union tracks the current provider set (and can SHRINK on delete).
pub(crate) fn reconcile_llm_egress_cnp(namespace: &str) -> Result<(), String> {
    let output = run_output(
        "kubectl",
        &[
            "get",
            "providers.sycophant.md",
            "-n",
            namespace,
            "-o",
            "jsonpath={range .items[*]}{.spec.format}{\"\\t\"}{.spec.baseUrl}{\"\\n\"}{end}",
        ],
    )?;
    let endpoints = endpoint_union(&parse_provider_list(&output));
    if endpoints.is_empty() {
        let _ = run_output(
            "kubectl",
            &[
                "delete",
                "ciliumnetworkpolicy",
                "llm-job-egress",
                "-n",
                namespace,
                "--ignore-not-found",
            ],
        );
        return Ok(());
    }
    let yaml = build_llm_egress_cnp_yaml(namespace, &endpoints);
    run_stdin("kubectl", &["apply", "-n", namespace, "-f", "-"], &yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_yaml(yaml: &str) -> serde_yaml::Value {
        serde_yaml::from_str(yaml).expect("builder output must be valid YAML")
    }

    fn public_ep(host: &str) -> ProviderEndpoint {
        ProviderEndpoint {
            host: host.into(),
            port: 443,
            is_private: false,
        }
    }

    fn private_ep(host: &str, port: u16) -> ProviderEndpoint {
        ProviderEndpoint {
            host: host.into(),
            port,
            is_private: true,
        }
    }

    #[test]
    fn provider_host_strips_scheme_path_port() {
        assert_eq!(
            provider_host("https://api.anthropic.com/v1").as_deref(),
            Some("api.anthropic.com")
        );
        assert_eq!(
            provider_host("http://localhost:8080/v1").as_deref(),
            Some("localhost")
        );
        assert_eq!(
            provider_host("api.mistral.ai/v1").as_deref(),
            Some("api.mistral.ai")
        );
        assert_eq!(provider_host(""), None);
        assert_eq!(provider_host("https://"), None);
    }

    #[test]
    fn provider_port_explicit_wins_else_scheme_default() {
        assert_eq!(provider_port("https://api.anthropic.com/v1"), 443);
        assert_eq!(provider_port("api.anthropic.com/v1"), 443); // scheme-less -> 443
        assert_eq!(provider_port("http://example.com/v1"), 80);
        assert_eq!(provider_port("http://192.168.65.254:11434/v1"), 11434);
        assert_eq!(provider_port("https://llm.internal:8443/v1"), 8443);
    }

    #[test]
    fn is_private_or_loopback_ip_classifies_ipv4_ranges() {
        for ip in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.0.1",
            "192.168.65.254",
            "169.254.1.1",
        ] {
            assert!(
                is_private_or_loopback_ip(ip),
                "{ip} should be private/loopback"
            );
        }
        for h in [
            "8.8.8.8",
            "172.32.0.1",
            "169.253.0.1",
            "api.anthropic.com",
            "localhost",
        ] {
            assert!(!is_private_or_loopback_ip(h), "{h} should NOT be private");
        }
    }

    #[test]
    fn canonical_base_url_returns_format_specific_endpoint() {
        assert_eq!(
            canonical_base_url("anthropic"),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(canonical_base_url("openai"), "https://api.openai.com/v1");
        assert!(canonical_base_url("gemini").contains("generativelanguage"));
        assert_eq!(canonical_base_url("unknown"), "");
    }

    #[test]
    fn endpoint_union_canonicalizes_and_dedups() {
        let providers = vec![
            ("anthropic".into(), None),                                  // canonical
            ("openai".into(), Some("https://api.mistral.ai/v1".into())), // baseUrl override
            ("openai".into(), Some("https://api.mistral.ai/v1".into())), // duplicate host
            ("unknown".into(), None),                                    // no host -> skipped
        ];
        let eps = endpoint_union(&providers);
        let hosts: Vec<&str> = eps.iter().map(|e| e.host.as_str()).collect();
        assert_eq!(hosts, vec!["api.anthropic.com", "api.mistral.ai"]);
        assert!(eps.iter().all(|e| e.port == 443 && !e.is_private));
    }

    #[test]
    fn endpoint_union_marks_private_ip_with_port() {
        let providers = vec![(
            "openai".into(),
            Some("http://192.168.65.254:11434/v1".into()),
        )];
        let eps = endpoint_union(&providers);
        assert_eq!(eps, vec![private_ep("192.168.65.254", 11434)]);
    }

    #[test]
    fn parse_provider_list_handles_absent_base_url() {
        let parsed = parse_provider_list("anthropic\thttps://api.anthropic.com/v1\nopenai\t\n");
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0],
            (
                "anthropic".into(),
                Some("https://api.anthropic.com/v1".into())
            )
        );
        assert_eq!(parsed[1], ("openai".into(), None));
        assert!(parse_provider_list("  \n").is_empty());
    }

    #[test]
    fn llm_cnp_selects_llm_job_and_allowlists_each_host() {
        let v = parse_yaml(&build_llm_egress_cnp_yaml(
            "ns",
            &[public_ep("api.anthropic.com"), public_ep("api.mistral.ai")],
        ));
        assert_eq!(v["kind"].as_str(), Some("CiliumNetworkPolicy"));
        assert_eq!(v["metadata"]["name"].as_str(), Some("llm-job-egress"));
        assert_eq!(
            v["spec"]["endpointSelector"]["matchLabels"]["app.kubernetes.io/component"].as_str(),
            Some("llm-job")
        );
        let dns = v["spec"]["egress"][0]["toPorts"][0]["rules"]["dns"]
            .as_sequence()
            .unwrap();
        let names: Vec<&str> = dns.iter().filter_map(|d| d["matchName"].as_str()).collect();
        assert!(names.contains(&"hangar-ctrl.ns.svc.cluster.local"));
        assert!(names.contains(&"api.anthropic.com"));
        assert!(names.contains(&"api.mistral.ai"));
        // toFQDNs:443 carries each host too (DNS->IP learning for the L4 rule).
        let egress = v["spec"]["egress"].as_sequence().unwrap();
        let fq = egress.last().unwrap();
        let fq_names: Vec<&str> = fq["toFQDNs"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|d| d["matchName"].as_str())
            .collect();
        assert!(fq_names.contains(&"api.anthropic.com"));
        assert_eq!(fq["toPorts"][0]["ports"][0]["port"].as_str(), Some("443"));
    }

    #[test]
    fn llm_cnp_empty_endpoints_has_dns_and_hangar_only() {
        let yaml = build_llm_egress_cnp_yaml("ns", &[]);
        assert!(
            !yaml.contains("toFQDNs"),
            "no toFQDNs when there are no hosts"
        );
        assert!(
            !yaml.contains("toCIDR"),
            "no toCIDR when there are no hosts"
        );
        let v = parse_yaml(&yaml);
        let egress = v["spec"]["egress"].as_sequence().unwrap();
        assert_eq!(egress.len(), 2, "DNS rule + hangar:9090 only");
        let dns = egress[0]["toPorts"][0]["rules"]["dns"]
            .as_sequence()
            .unwrap();
        assert_eq!(dns.len(), 1);
        assert_eq!(
            dns[0]["matchName"].as_str(),
            Some("hangar-ctrl.ns.svc.cluster.local")
        );
    }

    #[test]
    fn llm_cnp_private_ip_pins_cidr_not_fqdn() {
        let yaml = build_llm_egress_cnp_yaml("ns", &[private_ep("192.168.65.254", 11434)]);
        // Private-only set: pinned by toCIDR /32 on the parsed port; NO toFQDNs.
        assert!(!yaml.contains("toFQDNs"), "private IP must not use toFQDNs");
        let v = parse_yaml(&yaml);
        let egress = v["spec"]["egress"].as_sequence().unwrap();
        let cidr_rule = egress
            .iter()
            .find(|e| e.get("toCIDR").is_some())
            .expect("toCIDR rule present");
        let cidrs: Vec<&str> = cidr_rule["toCIDR"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|c| c.as_str())
            .collect();
        assert_eq!(cidrs, vec!["192.168.65.254/32"]);
        assert_eq!(
            cidr_rule["toPorts"][0]["ports"][0]["port"].as_str(),
            Some("11434")
        );
        assert_eq!(
            cidr_rule["toPorts"][0]["ports"][0]["protocol"].as_str(),
            Some("TCP")
        );
        // The IP must NOT leak into the DNS allowlist (it isn't DNS-resolved).
        let dns = egress[0]["toPorts"][0]["rules"]["dns"]
            .as_sequence()
            .unwrap();
        let names: Vec<&str> = dns.iter().filter_map(|d| d["matchName"].as_str()).collect();
        assert!(!names.contains(&"192.168.65.254"));
    }

    #[test]
    fn llm_cnp_mixed_public_and_private_split_correctly() {
        let yaml = build_llm_egress_cnp_yaml(
            "ns",
            &[
                public_ep("api.anthropic.com"),
                private_ep("192.168.65.254", 11434),
            ],
        );
        let v = parse_yaml(&yaml);
        let egress = v["spec"]["egress"].as_sequence().unwrap();
        // public -> toFQDNs:443 (and NOT the private IP)
        let fq = egress
            .iter()
            .find(|e| e.get("toFQDNs").is_some())
            .expect("toFQDNs for public");
        let fq_names: Vec<&str> = fq["toFQDNs"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|d| d["matchName"].as_str())
            .collect();
        assert_eq!(fq_names, vec!["api.anthropic.com"]);
        assert_eq!(fq["toPorts"][0]["ports"][0]["port"].as_str(), Some("443"));
        // private -> toCIDR:11434
        let cidr = egress
            .iter()
            .find(|e| e.get("toCIDR").is_some())
            .expect("toCIDR for private");
        assert_eq!(
            cidr["toPorts"][0]["ports"][0]["port"].as_str(),
            Some("11434")
        );
        // public host in DNS allowlist, private IP NOT
        let dns = egress[0]["toPorts"][0]["rules"]["dns"]
            .as_sequence()
            .unwrap();
        let names: Vec<&str> = dns.iter().filter_map(|d| d["matchName"].as_str()).collect();
        assert!(names.contains(&"api.anthropic.com"));
        assert!(!names.contains(&"192.168.65.254"));
    }

    #[test]
    fn llm_cnp_has_no_catchall_and_string_ports() {
        let yaml = build_llm_egress_cnp_yaml(
            "ns",
            &[
                public_ep("api.anthropic.com"),
                private_ep("10.0.0.9", 11434),
            ],
        );
        assert!(!yaml.contains(r#"matchName: "*""#));
        assert!(!yaml.contains("0.0.0.0/0"));
        assert!(!yaml.contains("world"));
        let v = parse_yaml(&yaml);
        let dns_port = &v["spec"]["egress"][0]["toPorts"][0]["ports"][0]["port"];
        assert_eq!(dns_port.as_str(), Some("53"));
        assert!(dns_port.as_u64().is_none(), "Cilium ports must be strings");
    }
}
