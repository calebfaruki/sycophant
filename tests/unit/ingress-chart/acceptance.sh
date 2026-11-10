#!/usr/bin/env bash
# Offline: renders the ingress chart and the cluster chart's ingress binding,
# and checks what each one emits. No cluster.
#
#   bash tests/unit/ingress-chart/acceptance.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE
source "$HERE/../lib.sh"

render() {
  local out="$1"
  shift
  capture "$out" helm template i "$ROOT/charts/sycophant-ingress" \
    -n sycophant-ingress "$@"
}
DOMAIN=(--set domain=ingress.example.com)
NODEPORT=(--set nodePort=31999)
VPN=(--set vpn.namespace=tenant-acme --set vpn.service=headscale
  --set vpn.port=8080 --set vpn.component=headscale)
render base "${DOMAIN[@]}" "${NODEPORT[@]}" "${VPN[@]}"
render no-domain "${NODEPORT[@]}" "${VPN[@]}"
render no-nodeport "${DOMAIN[@]}" "${VPN[@]}"
render unknown-key "${DOMAIN[@]}" "${NODEPORT[@]}" "${VPN[@]}" --set bogus=1
capture cluster helm template c "$ROOT/charts/sycophant-cluster" -n kyverno \
  --api-versions policies.kyverno.io/v1/ValidatingPolicy

python3 - "$WORK" <<'PY'
import json, sys, yaml

work = sys.argv[1]
passed = failed = 0
LE = "https://acme-v02.api.letsencrypt.org/directory"
ACME_HOST = "acme-v02.api.letsencrypt.org"
VPN_HOST = "headscale.tenant-acme.svc.cluster.local"
LABELS = {"app.kubernetes.io/part-of": "sycophant",
          "app.kubernetes.io/component": "ingress"}

def ok(name):
    global passed
    passed += 1
    print(f"ok   {name}")

def bad(name, why):
    global failed
    failed += 1
    print(f"FAIL {name}\n    {why}")

def load(out):
    if int(open(f"{work}/{out}.rc").read()) != 0:
        return None, open(f"{work}/{out}.err").read().strip()
    return [d
            for d in yaml.safe_load_all(open(f"{work}/{out}.yaml")) if d], None

def check(name, out, fn):
    docs, err = load(out)
    if docs is None:
        bad(name, err)
        return
    try:
        why = fn(docs)
    except Exception as e:
        why = f"{type(e).__name__}: {e}"
    if why:
        bad(name, why)
    else:
        ok(name)

def refuses(name, out, token):
    docs, err = load(out)
    if docs is not None:
        bad(name, "the chart rendered")
    elif token not in err:
        bad(name,
            f"the render failed, but not naming {token!r}: {err[-300:]}")
    else:
        ok(name)

def kinds(docs, kind):
    return [d for d in docs if d["kind"] == kind]

def ingress_labels(m):
    return all(m.get(k) == v for k, v in LABELS.items())

def caddy_pod(docs):
    deps = [d for d in kinds(docs, "Deployment")
            if ingress_labels(d["spec"]["template"]["metadata"]
                              .get("labels", {}))]
    if len(deps) != 1:
        raise AssertionError(
            f"want one Deployment whose pod carries {LABELS}, got {len(deps)}")
    return deps[0]["spec"]["template"]["spec"]

def caddyfile(docs):
    texts = [
        v for cm in kinds(docs, "ConfigMap")
        for v in (cm.get("data") or {}).values()
    ]
    if len(texts) != 1:
        raise AssertionError(
            f"want one ConfigMap entry holding the Caddyfile, got {len(texts)}")
    text = texts[0]
    lines = [w for w in (
        l.split("#", 1)[0].split()
        for l in text.splitlines()
    ) if w]
    has = lambda *words: list(words) in lines
    problems = [
        not has("disable_http_challenge") and "no disable_http_challenge",
        [l[:2] for l in lines if l[0] == "issuer"] != [["issuer", "acme"]]
        and "want exactly one issuer, acme",
        [l for l in lines if l[0] == "dir"] != [["dir", LE]]
        and f"want exactly one dir, {LE}",
        not has("https_port", "8443") and "no https_port 8443",
        not has("auto_https", "disable_redirects")
        and "no auto_https disable_redirects",
        not has("admin", "off") and "no admin off",
    ]
    return [p for p in problems if p] or None
check(
    "the Caddyfile turns off the HTTP challenge and the admin API, uses only "
    "the Let's Encrypt issuer, serves on 8443 and opens no redirect listener",
    "base", caddyfile)

def pod_envelope(docs):
    spec = caddy_pod(docs)
    psc = spec.get("securityContext") or {}
    conts = (spec.get("initContainers") or []) + spec["containers"]
    sc = lambda c: c.get("securityContext") or {}
    pvcs = {d["metadata"]["name"] for d in kinds(docs, "PersistentVolumeClaim")}
    on_pvc = {
        v["name"] for v in spec.get("volumes") or []
        if (v.get("persistentVolumeClaim") or {}).get("claimName") in pvcs
    }
    data_mounts = [
        m for c in spec["containers"] for m in c.get("volumeMounts")
        or [] if m.get("mountPath") == "/data"
    ]
    env = {
        e["name"]: e.get("value") for c in spec["containers"]
        for e in c.get("env") or []
    }
    problems = [
        spec.get("automountServiceAccountToken") is not False
        and "automountServiceAccountToken",
        any(spec.get(h) for h in ("hostNetwork", "hostPID", "hostIPC"))
        and "host namespace",
        (
            psc.get("runAsNonRoot"), psc.get("runAsUser"),
            psc.get("runAsGroup"), psc.get("fsGroup")
        ) != (True, 65532, 65532, 65532)
        and f"pod securityContext {psc}",
        (psc.get("seccompProfile") or {}).get("type") != "RuntimeDefault"
        and "seccompProfile",
        any((
            sc(c).get("readOnlyRootFilesystem"),
            sc(c).get("allowPrivilegeEscalation")
        ) != (True, False)
            or "ALL" not in (sc(c).get("capabilities") or {}).get("drop", [])
            or (sc(c).get("capabilities") or {}).get("add")
            or sc(c).get("privileged") for c in conts)
        and "container securityContext",
        any(p.get("hostPort") for c in conts for p in c.get("ports") or [])
        and "hostPort",
        any("hostPath" in v for v in spec.get("volumes") or [])
        and "hostPath volume",
        (len(data_mounts) != 1 or data_mounts[0]["name"] not in on_pvc)
        and f"/data is not on the chart's PVC: {data_mounts}",
        env.get("XDG_DATA_HOME") != "/data"
        and f"XDG_DATA_HOME {env.get('XDG_DATA_HOME')!r}",
    ]
    return [p for p in problems if p] or None
check(
    "the Caddy pod carries the ingress labels and the hardened envelope, with "
    "no host reach and its data on the PVC",
    "base", pod_envelope)

def default_image(docs):
    got = [
        (c.get("image"), c.get("imagePullPolicy"))
        for c in caddy_pod(docs)["containers"]
    ]
    return (None if got == [("sycophant-caddy:local", "IfNotPresent")]
            else f"got {got}")
check("the default image is sycophant-caddy:local with IfNotPresent", "base",
      default_image)

def node_port(docs):
    got = [
        (
            s["spec"].get("type"),
            [p.get("nodePort") for p in s["spec"].get("ports") or []]
        )
        for s in kinds(docs, "Service")
    ]
    return (None if got == [("NodePort", [31999])]
            else f"want one NodePort Service on 31999, got {got}")
check("the Service NodePort is the nodePort value", "base", node_port)

def egress(docs):
    pol = [
        p for p in kinds(docs, "CiliumNetworkPolicy")
        if ingress_labels((p["spec"].get("endpointSelector") or {})
                          .get("matchLabels", {}))
    ]
    rules = [r for p in pol for r in p["spec"].get("egress") or []]
    def is_dns(r):
        dns = [
            n for t in r.get("toPorts") or []
            for n in (t.get("rules") or {}).get("dns") or []
        ]
        ports = {
            p["port"] for t in r.get("toPorts") or []
            for p in t.get("ports") or []
        }
        return (set(r) == {"toEndpoints", "toPorts"}
                and all((e.get("matchLabels") or {}).get("k8s-app")
                        == "kube-dns" for e in r["toEndpoints"])
                and ports == {"53"} and all((t.get("rules") or {}).get("dns")
                                            for t in r["toPorts"])
                and sorted(n.get("matchName", "") for n in dns)
                == sorted([ACME_HOST, VPN_HOST])
                and all(set(n) == {"matchName"} for n in dns))
    def is_acme(r):
        return (set(r) == {"toFQDNs", "toPorts"}
                and r["toFQDNs"] == [{"matchName": ACME_HOST}]
                and r["toPorts"] == [{"ports": [{
                    "port": "443",
                    "protocol": "TCP"
                }]}])
    def is_vpn(r):
        return r == {
            "toEndpoints": [{
                "matchLabels": {
                    "k8s:io.kubernetes.pod.namespace": "tenant-acme",
                    "app.kubernetes.io/component": "headscale"
                }
            }],
            "toPorts": [{"ports": [{"port": "8080", "protocol": "TCP"}]}]
        }
    found = sorted(
        name for r in rules
        for name, f in (("dns", is_dns), ("acme", is_acme),
                        ("vpn", is_vpn)) if f(r)
    )
    return None if found == ["acme", "dns", "vpn"] and len(rules) == 3 else \
        (
            "want exactly the L7 kube-dns rule for the Let's Encrypt and VPN "
            "names, the Let's Encrypt FQDN on 443 and the VPN pod on its port; "
            f"got {json.dumps(rules)}"
        )
check(
    "Caddy egresses only to kube-dns resolving the Let's Encrypt and VPN "
    "names, the Let's Encrypt name on 443, and the VPN pod on its port",
    "base", egress)

refuses("the schema refuses a missing domain", "no-domain",
        "missing property 'domain'")
refuses("the schema refuses a missing nodePort", "no-nodeport",
        "missing property 'nodePort'")
refuses("the schema refuses an unknown key", "unknown-key",
        "additional properties 'bogus' not allowed")

def binding(docs):
    b = [
        d for d in kinds(docs, "ValidatingAdmissionPolicyBinding")
        if d["metadata"]["name"] == "sycophant-ingress-pod-binding"
    ]
    if len(b) != 1:
        return f"want one sycophant-ingress-pod-binding, got {len(b)}"
    spec = b[0]["spec"]
    sel = spec.get("matchResources", {}).get("namespaceSelector")
    exact = (
        sel
        == {"matchLabels": {"kubernetes.io/metadata.name":
                            "sycophant-ingress"}}
    )
    got = (spec.get("policyName"), spec.get("validationActions"), exact)
    return (None if got == ("cluster-gvisor-pod-policy", ["Deny"], True)
            else "policy, actions, selects only "
            f"sycophant-ingress: {got} {sel}")
check(
    "the cluster chart binds cluster-gvisor-pod-policy to exactly the "
    "sycophant-ingress namespace",
    "cluster", binding)

print(f"\n{passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
PY
