#!/usr/bin/env bash
# Offline: renders the tenant charts, the object store and the cluster chart,
# and checks what each one emits. No cluster.
#
#   bash tests/unit/tenant-chart/acceptance.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE
source "$HERE/../lib.sh"

# editorconfig-checker-disable-next-line
LOCAL='{"format":"openai","model":"liquid/lfm2.5-8b-a1b","server.image":"llama-server:local","server.weights":"ghcr.io/example/weights:1","server.weightsPath":"/weights/model.gguf"}'
# editorconfig-checker-disable-next-line
REMOTE='{"format":"openai","model":"deepseek/deepseek-v4-flash","baseUrl":"https://openrouter.ai/api/v1","image":"inference-runtime:local","secret":"llm-openrouter"}'
# editorconfig-checker-disable-next-line
TOOLSET='{"image":"ghcr.io/sycophant-md/git@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","keepalive":"false","tool.git-status.description":"Show the working tree status.","tool.git-commit.description":"Commit staged changes.","tool.git-commit.arg.message.type":"string","tool.git-commit.arg.message.required":"true","tool.git-commit.arg.message.env":"MESSAGE"}'
# editorconfig-checker-disable-next-line
AGENT='{"toolsets":"git,ssh","model":"a","storage":"5Gi","archives":"3","secret.ssh.demo-key":"/home/agent/.ssh/id","secret.ssh.local-key":"/home/agent/.ssh/local","egress.ssh":"github.com,gitlab.com"}'
CHANNEL='{"loginServer":"http://headscale:8080"}'
SUBJECT="https://issuer.example#alice"
HASH="$(printf '%s' "$SUBJECT" | openssl dgst -sha256 | awk '{print $NF}' |
  cut -c1-10)"
# Offline there is no cluster, so a listed agent never exists: the user
# chart must refuse it. The positive shape is rendered with no agents.
USER_COMMAND="$(printf '{"user":"%s","handle.slack":"U12345"}' "$SUBJECT")"
USER_MISSING_AG="$(printf \
  '{"user":"%s","agents":"alpha,beta","handle.slack":"U12345"}' "$SUBJECT")"
# editorconfig-checker-disable-next-line
REMOTE_IP='{"format":"openai","model":"m","baseUrl":"http://10.42.0.9:8080/v1","image":"inference-runtime:local"}'
readonly LOCAL REMOTE REMOTE_IP TOOLSET AGENT CHANNEL SUBJECT HASH \
  USER_COMMAND USER_MISSING_AG

render() {
  local out="$1" chart="$2"
  shift 2
  capture "$out" helm template t "$ROOT/charts/sycophant-$chart" \
    -n tenant-acme "$@"
}
render bare tenant --set namespace.create=false
render ns tenant --show-only templates/tenant-ns.yaml
render channel channel --set name=app --set-json "command=$CHANNEL"
render headscale tenant --set namespace.create=false \
  --set headscale.enabled=true --set headscale.serverUrl=http://localhost:8080
render acme tenant --set namespace.create=false --set headscale.enabled=true \
  --set headscale.serverUrl=http://localhost:8080 \
  --set headscale.acme.enabled=true --set headscale.acme.email=op@example.com
render no-magic tenant --set namespace.create=false \
  --set headscale.enabled=true --set headscale.serverUrl=http://localhost:8080 \
  --set headscale.magicDns=false
render idp tenant --set namespace.create=false \
  --set idp.issuer=https://dex.example.com --set idp.audience=syco-client \
  --set idp.certificateAuthority=test-ca-bundle
render local inference --set name=local --set-json "command=$LOCAL"
render remote inference --set name=deepseek --set-json "command=$REMOTE"
render remote-ip inference --set name=ipdirect --set-json "command=$REMOTE_IP"
render toolset toolset --set name=git --set-json "command=$TOOLSET"
render agent agent --set name=one --set-json "command=$AGENT"
render agbase agent --set name=one --set-json 'command={"model":"a"}'
# editorconfig-checker-disable-next-line
render subagent agent --set name=sub --set-json 'command={"toolsets":"ssh","model":"a","parent":"one","secret.ssh.sub-key":"/home/agent/.ssh/id","egress.ssh":"example.com"}'
render user user --set-string name="$HASH" --set-json "command=$USER_COMMAND"
render user-missing-ag user --set-string name="$HASH" \
  --set-json "command=$USER_MISSING_AG"
render store objectstore
# editorconfig-checker-disable
for m in override:'{"server.weightsPath":"/opt/m.gguf","server.contextBudget":"8192","server.slots":"1","server.outputCap":"512","server.threads":"2","server.memory":"4Gi","server.cpu":"2"}' \
  mount:'{"server.weightsDelivery":"mount"}' \
  tpl-copy:'{"server.chatTemplate":"TEMPLATE-BODY"}' \
  tpl-mount:'{"server.weightsDelivery":"mount","server.chatTemplate":"TEMPLATE-BODY"}' \
  single-true:'{"server.singleNodeException":"true"}' \
  single-false:'{"server.singleNodeException":"false"}' \
  think-false:'{"server.thinking":"false"}' \
  think-true:'{"server.thinking":"true"}'; do
  render "local-${m%%:*}" inference --set name=local \
    --set-json "command=$(printf '%s' "$LOCAL" | jq -c ". + ${m#*:}")"
done
# editorconfig-checker-enable

# The channel the walkthrough runs, and the cluster chart whose rules fence it.
render channel-tailnet channel --set name=tailnet --set-json "command=$CHANNEL"
capture cluster helm template c "$ROOT/charts/sycophant-cluster" -n kyverno \
  --api-versions policies.kyverno.io/v1/ValidatingPolicy

# The pod-hardening VAP against the tailnet channel pod: once as rendered, and
# once with its runtimeClassName removed.
python3 - "$WORK" <<'PY'
import os, sys, yaml
work = sys.argv[1]
case = os.path.join(work, "channel-hardening")
os.makedirs(case)
load = lambda f: [d
                  for d in yaml.safe_load_all(open(os.path.join(work, f))) if d]
vap = [
    d for d in load("cluster.yaml")
    if d["kind"] == "ValidatingAdmissionPolicy"
    and d["metadata"]["name"] == "cluster-gvisor-pod-policy"
]
dep = [
    d for d in load("channel-tailnet.yaml") if d["kind"] == "Deployment"
    and d["metadata"]["name"] == "ch-tailnet"
]
if not vap or not dep:
    sys.exit(0)
binding = {
    "apiVersion": "admissionregistration.k8s.io/v1",
    "kind": "ValidatingAdmissionPolicyBinding",
    "metadata": {"name": "tenant-acme-sycophant-pod-binding"},
    "spec": {
        "policyName": "cluster-gvisor-pod-policy",
        "validationActions": ["Deny"]
    }
}
yaml.safe_dump_all(
    vap + [binding],
    open(os.path.join(case, "policy.yaml"), "w")
)
tpl = dep[0]["spec"]["template"]
pods = []
for name, strip in (("ch-tailnet-as-rendered", False),
                    ("ch-tailnet-no-runtime-class", True)):
    spec = dict(tpl["spec"])
    if strip:
        spec.pop("runtimeClassName", None)
    pods.append({
        "apiVersion": "v1", "kind": "Pod", "spec": spec,
        "metadata": {
            "name": name, "namespace": "tenant-acme",
            "labels": tpl["metadata"]["labels"]
        }
    })
yaml.safe_dump_all(pods, open(os.path.join(case, "pods.yaml"), "w"))
yaml.safe_dump(
    {
        "apiVersion": "cli.kyverno.io/v1alpha1", "kind": "Test",
        "metadata": {"name": "channel-hardening"}, "policies": ["policy.yaml"],
        "resources": ["pods.yaml"],
        "results": [{
            "policy": "cluster-gvisor-pod-policy",
            "isValidatingAdmissionPolicy": True, "kind": "Pod",
            "result": r, "resources": [f"tenant-acme/{n}"]
        }
                    for n, r in (
                        ("ch-tailnet-as-rendered", "pass"),
                        ("ch-tailnet-no-runtime-class", "fail")
                    )]
    },
    open(os.path.join(case, "kyverno-test.yaml"), "w")
)
PY
if [ -f "$WORK/channel-hardening/kyverno-test.yaml" ]; then
  results channel-hardening >/dev/null
fi

python3 - "$WORK" "$REMOTE" "$TOOLSET" "$AGENT" "$SUBJECT" "$HASH" \
  "$ROOT" <<'PY'
import json, os, re, sys, yaml

work, remote, toolset, agent = (
    sys.argv[1], json.loads(sys.argv[2]),
    json.loads(sys.argv[3]), json.loads(sys.argv[4])
)
subject, hash10, root = sys.argv[5], sys.argv[6], sys.argv[7]
passed = failed = 0

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

def docs_of(out):
    docs, err = load(out)
    if docs is None:
        raise RuntimeError(f"{out}: {err}")
    return docs

def check(name, out, fn):
    docs, err = load(out) if out else ([], None)
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

def objects(docs):
    return sorted(f"{d['kind']}/{d['metadata']['name']}" for d in docs)

def get(docs, kind, name):
    return (next((d for d in docs if d["kind"] == kind
                  and d["metadata"]["name"] == name), None))

def pod(d):
    return d["spec"]["template"]

def env_of(spec, container=None):
    return {e["name"]: e.get("value") for c in spec["containers"]
            if container in (None, c["name"]) for e in c.get("env") or []}

def selects(match, labels):
    return bool(match) and all(labels.get(k) == v for k, v in match.items())

def ports(rule):
    return {int(p["port"]) for t in rule.get("toPorts") or []
            for p in t.get("ports") or []}

def cnps(docs):
    return [d for d in docs if d["kind"] == "CiliumNetworkPolicy"]

def argv(d):
    c = next(c for c in pod(d)["spec"]["containers"] if c["name"] == "server")
    return c.get("command", []) + c.get("args", [])

def pair(av, flag):
    return av[av.index(flag) + 1] if flag in av else None

def model_cm(docs, key):
    for d in docs:
        labels = d["metadata"].get("labels") or {}
        if d["kind"] == "ConfigMap" and d["metadata"]["name"] == f"inf-{key}" \
                and labels.get("sycophant.md/type") == "model" and labels.get(
                    "app.kubernetes.io/name") == key:
            return d
    return None

BASE = ["CiliumNetworkPolicy/default-deny-egress",
        "CiliumNetworkPolicy/install-wait-egress",
        "CiliumNetworkPolicy/relay-egress", "CiliumNetworkPolicy/relay-ingress",
        "ConfigMap/toolset-scheduling",
        "Deployment/relay-controller", "Job/install-wait",
        "Role/install-wait", "Role/relay-controller",
        "RoleBinding/install-wait", "RoleBinding/relay-controller",
        "Service/relay-controller", "ServiceAccount/install-wait",
        "ServiceAccount/relay-controller"]
check("base chart renders only the tenant's shared objects", "bare",
      lambda docs: None if objects(docs) == BASE
      else f"want {BASE}, got {objects(docs)}")

def relay_idp_env(docs):
    env = env_of(pod(get(docs, "Deployment", "relay-controller"))["spec"])
    want = {"IDP_ISSUER": "https://dex.example.com",
            "IDP_AUDIENCE": "syco-client",
            "IDP_CERTIFICATE_AUTHORITY": "test-ca-bundle"}
    got = {k: env.get(k) for k in want}
    return None if got == want else f"want {want}, got {got}"
check("the relay container carries the idp issuer, audience and CA", "idp",
      relay_idp_env)

def relay_rules(docs):
    d = get(docs, "CiliumNetworkPolicy", "relay-ingress")
    return (d or {}).get("spec", {}).get("ingress") or []

def relay_fence(docs):
    relay = (pod(get(
        docs, "Deployment",
        "relay-controller"
    ))["metadata"]["labels"])
    fences = [d["metadata"]["name"] for d in cnps(docs)
              if d["spec"].get("ingress")
              and selects(d["spec"]["endpointSelector"]
                          .get("matchLabels"), relay)]
    if fences != ["relay-ingress"]:
        return f"want only relay-ingress fencing the relay, got {fences}"
    key = lambda r: json.dumps(r, sort_keys=True)
    got = sorted(
        key((
            [
                e.get("matchLabels")
                for e in r.get("fromEndpoints") or []
            ],
            [p for t in r.get("toPorts") or [] for p in t.get("ports") or []]
        ))
        for r in relay_rules(docs)
    )
    channel = lambda cls: {
        "app.kubernetes.io/component": "channel",
        "sycophant.md/channel-class": cls,
        "app.kubernetes.io/part-of": "sycophant"
    }
    want = sorted(key(r) for r in [
        ([channel("transport")], [{"port": "9091", "protocol": "TCP"}]),
        (
            [{"app.kubernetes.io/component": "harness",
              "app.kubernetes.io/part-of": "sycophant"}],
            [{"port": "9090", "protocol": "TCP"}]
        ),
        ([channel("principal")], [{"port": "9092", "protocol": "TCP"}]),
    ])
    return None if got == want else f"want {want}, got {got}"
check(
    "relay-ingress alone fences the relay: transport channel on 9091, harness "
    "on 9090, principal channels on 9092",
    "bare", relay_fence)

def listeners(docs):
    relay = get(docs, "Deployment", "relay-controller")
    labels = pod(relay)["metadata"]["labels"]
    named = {
        p["name"]: p["containerPort"]
        for c in pod(relay)["spec"]["containers"]
        for p in c.get("ports") or []
    }
    svc = get(docs, "Service", "relay-controller") or {}
    published = {
        named.get(p["targetPort"], p["targetPort"])
        for p in svc.get("spec", {}).get("ports") or []
    }
    admit = {
        p for d in cnps(docs)
        if selects(d["spec"]["endpointSelector"].get("matchLabels"), labels)
        for r in d["spec"].get("ingress") or [] for p in ports(r)
    }
    dial = {p for d in cnps(docs) for r in d["spec"].get("egress") or []
            if any(
                selects(e.get("matchLabels"), labels)
                for e in r.get("toEndpoints") or []
            )
            for p in ports(r)}
    gaps = {p: [k for k, s in (
        ("service", published), ("ingress", admit),
        ("egress", dial)
    ) if p not in s]
            for p in sorted(named.values())}
    gaps = {p: g for p, g in gaps.items() if g}
    return (None if named and not gaps
            else f"listeners {sorted(named.values())} missing {gaps}")
check("every relay listener is published, admitted and dialed", None,
      lambda _: listeners(docs_of("bare") + docs_of("agbase")
                          + docs_of("channel")))

def channel_shape(docs):
    deps = [d for d in docs if d["kind"] == "Deployment"
            and pod(d)["metadata"]["labels"].get("app.kubernetes.io/component")
            == "channel"]
    if len(deps) != 1:
        return f"want one channel Deployment, got {objects(deps)}"
    d = deps[0]
    spec = pod(d)["spec"]
    env = env_of(spec)
    problems = [
        (d["spec"].get("replicas"), d["spec"].get("strategy", {}).get("type"))
        != (1, "Recreate") and "replicas/strategy",
        spec.get("runtimeClassName") != "gvisor" and "runtimeClassName",
        spec.get("automountServiceAccountToken") is not False
        and "automountServiceAccountToken",
        (spec.get("securityContext") or {}).get("seccompProfile", {})
        .get("type") != "RuntimeDefault" and "seccompProfile",
        env.get("TS_USERSPACE") != "true" and "TS_USERSPACE",
        any(((c.get("securityContext") or {}).get("capabilities") or {})
            .get("add") for c in spec["containers"]) and "capabilities.add",
        any("persistentVolumeClaim" in v for v in spec.get("volumes") or [])
        and "persistentVolumeClaim",
    ]
    return [p for p in problems if p] or None
check("app channel pod is a hardened singleton", "channel", channel_shape)

def channel_class(docs):
    d = get(docs, "Deployment", "ch-app")
    labels = pod(d)["metadata"]["labels"]
    if ((
        labels.get("sycophant.md/channel-class"),
        labels.get("sycophant.md/channel")
    ) != ("transport", "app")):
        return f"channel labels {labels}"
    pols = [p for p in cnps(docs)
            if selects(
                p["spec"]["endpointSelector"]
                .get("matchLabels"), labels
            )]
    if not pols or any(
        not p["spec"].get("egress") or p["spec"].get("ingress")
        or "all" in json.dumps([r.get("toEntities")
                                for r in p["spec"]["egress"]]) for p in pols
    ):
        return (
            "want only fail-closed egress policies selecting the channel, got "
            f"{[p['spec'] for p in pols]}")
    every = [
        p for out in ("channel", "bare", "agbase") for p in docs_of(out)
        if p["kind"] in ("CiliumNetworkPolicy", "NetworkPolicy")
    ]
    def selector_keys(spec):
        sels = [spec.get("endpointSelector"), spec.get("podSelector")]
        sels += [
            e
            for r in (spec.get("egress") or []) +
            (spec.get("ingress") or [])
            for e in (r.get("toEndpoints") or []) +
            (r.get("fromEndpoints") or [])
        ]
        return {k for s in sels if s for k in list((s.get("matchLabels") or {}))
                + [x.get("key") for x in s.get("matchExpressions") or []]}
    keyed = [
        p["metadata"]["name"] for p in every
        if "sycophant.md/channel" in selector_keys(p["spec"])
    ]
    return keyed and f"policies keyed on the channel label: {keyed}"
check("app channel is transport class and policies key on the class", "channel",
      channel_class)

def headscale(_):
    def hosts(out):
        p = get(docs_of(out), "CiliumNetworkPolicy", "headscale-egress")
        if p is None:
            return None
        eg = p["spec"]["egress"]
        dns = sorted(
            n.get("matchName") or "pattern" for r in eg
            for t in r.get("toPorts") or []
            for n in (t.get("rules") or {}).get("dns") or []
        )
        fqdn = sorted(n.get("matchName") or "pattern" for r in eg
                      for n in r.get("toFQDNs") or [])
        return (p["spec"]["endpointSelector"]["matchLabels"]
                .get("app.kubernetes.io/component"), dns, fqdn)
    cp, acme = "controlplane.tailscale.com", "acme-v02.api.letsencrypt.org"
    got = {o: hosts(o) for o in ("headscale", "acme", "bare")}
    want = {"headscale": ("headscale", [cp], [cp]),
            "acme": ("headscale", [acme, cp], [acme, cp]),
            "bare": None}
    return None if got == want else f"want {want}, got {got}"
check("headscale egress reaches only its control endpoints",
      None, headscale)

def magic_dns(_):
    def value(out):
        cm = get(docs_of(out), "ConfigMap", "headscale-config")
        return (cm
                and yaml.safe_load(cm["data"][
                    "config.yaml"])["dns"]["magic_dns"])
    got = {o: value(o) for o in ("headscale", "no-magic")}
    return (None if got == {"headscale": True, "no-magic": False}
            else f"magic_dns {got}")
check("headscale magic_dns defaults on and turns off with \"false\"",
      None, magic_dns)

def harness_reaches_inference(docs):
    p = get(docs, "CiliumNetworkPolicy", "ag-one-egress")
    if p is None:
        return "ag-one-egress missing"
    eg = p["spec"].get("egress") or []
    warm = any(
        e.get("matchLabels")
        == {"app.kubernetes.io/component": "inference",
            "app.kubernetes.io/part-of": "sycophant"}
        for r in eg for e in r.get("toEndpoints") or []
    )
    dns = [
        n for r in eg for t in r.get("toPorts") or []
        for n in (t.get("rules") or {}).get("dns") or []
    ]
    want = [
        {"matchPattern": "inf-*.tenant-acme.svc.cluster.local"},
        {"matchPattern": "*.ag-one-capability.tenant-acme.svc.cluster.local"},
        {"matchName": "relay-controller.tenant-acme.svc.cluster.local"},
        {"matchName": "seaweedfs.sycophant-system.svc.cluster.local"},
    ]
    def reaches(labels, port):
        return (any(port in ports(r) and any(
            e.get("matchLabels") == labels
            for e in r.get("toEndpoints") or []
        ) for r in eg))
    relay = (reaches({"app.kubernetes.io/name": "relay-controller",
                      "app.kubernetes.io/part-of": "sycophant"}, 9090))
    store = reaches({
        "app.kubernetes.io/component": "objectstorage",
        "app.kubernetes.io/part-of": "sycophant",
        "k8s:io.kubernetes.pod.namespace": "sycophant-system"
    }, 8333)
    return (None if warm and relay and store
            and sorted(dns, key=str) == sorted(want, key=str)
            else f"warm={warm} relay={relay} store={store} dns={dns}")
check(
    "harness egress reaches the inference pods, the relay on 9090 and the "
    "store on 8333, and resolves only their names",
    "agbase", harness_reaches_inference)

def harness_dials_tool_pods(docs):
    p = get(docs, "CiliumNetworkPolicy", "ag-one-egress")
    if p is None:
        return "ag-one-egress missing"
    ingress = [r for r in p["spec"].get("ingress") or [] if 9090 in ports(r)]
    dials = any(
        9090 in ports(r)
        and any((e.get("matchLabels") or {})
                .get("app.kubernetes.io/component") == "capability-job"
                for e in r.get("toEndpoints") or [])
        for r in p["spec"].get("egress") or []
    )
    relay = [
        [e.get("matchLabels") for e in r.get("fromEndpoints") or []]
        for r in p["spec"].get("ingress") or [] if 9091 in ports(r)
    ]
    want = [[{"app.kubernetes.io/name": "relay-controller",
              "app.kubernetes.io/part-of": "sycophant"}]]
    return (None if not ingress and dials and relay == want
            else f"ingress on 9090 {ingress}, egress to capability-job on 9090 "
            f"{dials}, 9091 from {relay}")
check(
    "harness admits only the relay on 9091, nothing on 9090, and dials "
    "capability-job pods on 9090",
    "agbase", harness_dials_tool_pods)

def local_server(docs):
    want = ["CiliumNetworkPolicy/inf-local", "ConfigMap/inf-local",
            "Deployment/inf-local", "Service/inf-local"]
    if objects(docs) != want or not model_cm(docs, "local"):
        return f"want {want}, got {objects(docs)}"
    svc = get(docs, "Service", "inf-local")["spec"]
    labels = pod(get(docs, "Deployment", "inf-local"))["metadata"]["labels"]
    if (svc.get("type") != "ClusterIP"
        or not selects(svc.get("selector"), labels)):
        return f"Service {svc} does not select pod labels {labels}"
check(
    "local command renders the model server and its model ConfigMap, no "
    "provider egress",
    "local", local_server)

check(
    "remote command renders only its model ConfigMap and provider egress",
    "remote",
    lambda docs: None if objects(docs) == [
        "CiliumNetworkPolicy/inf-deepseek",
        "ConfigMap/inf-deepseek"
    ]
    and (model_cm(docs, "deepseek") or {}).get("data") == remote
    else f"got {objects(docs)} "
    f"{[d.get('data') for d in docs if d['kind'] == 'ConfigMap']}")

def toolset_cm(docs):
    if objects(docs) != ["ConfigMap/ts-git"]:
        return f"want only ConfigMap/ts-git, got {objects(docs)}"
    d = docs[0]
    labels = d["metadata"].get("labels") or {}
    if ((labels.get("sycophant.md/type"), labels.get("app.kubernetes.io/name"))
        != ("toolset", "git")):
        return f"labels {labels}"
    return None if d.get("data") == toolset else f"data {d.get('data')}"
check(
    "toolset command renders only its toolset ConfigMap carrying the "
    "command data",
    "toolset", toolset_cm)

def harness_env(docs):
    d = get(docs, "Deployment", "ag-one")
    return env_of(pod(d)["spec"], "harness") if d else {}

def command_env(docs):
    env = harness_env(docs)
    secrets = json.loads(env.get("TOOLSET_SECRETS") or "null")
    want = {k: v for k, v in agent.items() if k.startswith("secret.")}
    got = (env.get("TOOLSETS"), env.get("MODEL"), secrets)
    return None if got == ("git,ssh", "a", want) else \
        f"TOOLSETS, MODEL, TOOLSET_SECRETS {got}"
check(
    "agent command toolsets, its one model and its toolsets' Secrets reach "
    "its harness env",
    "agent", command_env)

def no_stdlib_default(docs):
    env = harness_env(docs)
    cm = get(docs, "ConfigMap", "ag-one-toolset-secrets")
    if (not get(docs, "Deployment", "ag-one") or env.get("TOOLSETS", "") != ""
        or cm is None or cm.get("data")):
        return (
            f"TOOLSETS {env.get('TOOLSETS')!r}, ag-one-toolset-secrets "
            f"{cm and cm.get('data')}")
check("an agent command naming no toolsets lists none and approves no Secret",
      "agbase", no_stdlib_default)

def archives_env(want):
    def fn(docs):
        got = harness_env(docs).get("ARCHIVES")
        return None if got == want else f"ARCHIVES {got!r}, want {want!r}"
    return fn
check("an agent command with no 'archives' gives its harness ARCHIVES=10",
      "agbase", archives_env("10"))
check("an agent command's 'archives' 3 reaches its harness env as ARCHIVES=3",
      "agent", archives_env("3"))

def command_toolset_objects(docs):
    cm = get(docs, "ConfigMap", "ag-one-toolset-secrets") or {"metadata": {}}
    if ((
        cm.get("data"),
        (cm["metadata"].get("labels") or {}).get("sycophant.md/type")
    )
        != ({"demo-key": "ssh", "local-key": "ssh"}, "toolset-secrets")):
        return f"ag-one-toolset-secrets {cm}"
    egress = [p for p in cnps(docs)
              if (p["metadata"].get("labels") or {}).get("sycophant.md/type")
              == "toolset-egress"]
    if [p["metadata"]["name"] for p in egress] != ["ag-one-toolset-ssh-egress"]:
        return (
            "want one toolset-egress CNP ag-one-toolset-ssh-egress (none for "
            f"git), got {[p['metadata']['name'] for p in egress]}")
    spec = egress[0]["spec"]
    sel = (spec.get("endpointSelector") or {}).get("matchLabels") or {}
    want_sel = {"app.kubernetes.io/part-of": "sycophant",
                "sycophant.md/agent": "one", "sycophant.md/toolset": "ssh"}
    if sel != want_sel:
        return f"endpointSelector {sel}, want {want_sel}"
    rules = spec.get("egress") or []
    dns = sorted(
        n.get("matchName") or "pattern" for r in rules
        if 53 in ports(r)
        for t in r.get("toPorts") or []
        for n in (t.get("rules") or {}).get("dns") or []
    )
    fqdns = sorted(
        n.get("matchName") or "pattern" for r in rules
        if ports(r) == {443} for n in r.get("toFQDNs") or []
    )
    if ((dns, fqdns) != (
        ["github.com", "gitlab.com"],
        ["github.com", "gitlab.com"]
    )):
        return f"dns matchName on :53 {dns}, toFQDNs matchName on 443 {fqdns}"
    return None
check(
    "an agent command renders its Secret allowlist and one egress policy per "
    "toolset with domains",
    "agent", command_toolset_objects)

# The init container calls the harness image's /bin/rclone with no shell. It
# syncs the folder keyed by the agent service account's token subject, and the
# source ends with "/" so rclone lists the folder rather than HEADing a bare
# key the read grant denies.
def command_instructions_sync(docs):
    spec = pod(get(docs, "Deployment", "ag-one"))["spec"]
    img = next(c["image"] for c in spec["containers"] if c["name"] == "harness")
    sync = " ".join(a for c in spec.get("initContainers") or []
                    if c["name"] == "instructions-sync"
                    and c.get("image") == img
                    for a in (c.get("command") or []) + (c.get("args") or []))
    want = (
        r"^/bin/rclone sync os:sycophant-instructions/system:serviceaccount:"
        r"tenant-acme:ag-one/(?=[\"'\s]|$)")
    if not re.search(want, sync):
        return (
            "want the harness image's /bin/rclone sync "
            "os:sycophant-instructions/system:serviceaccount:tenant-acme:"
            f"ag-one/ (trailing slash); got {sync!r}")
    vol = (next((
        v for v in spec.get("volumes") or []
        if v["name"] == "instructions-one"
    ), None))
    if vol is None or set(vol) != {"name", "emptyDir"}:
        return f"the instructions volume must be an emptyDir, got {vol}"
check(
    "an agent command syncs instructions with the harness image's "
    "/bin/rclone, no shell, from "
    "<bucket>/system:serviceaccount:<ns>:ag-<agent>/ into an emptyDir",
    "agent", command_instructions_sync)

def command_workspace_size(docs):
    pvc = get(docs, "PersistentVolumeClaim", "ag-one-workspace")
    size = ((pvc or {}).get("spec", {}).get("resources", {}).get("requests", {})
            .get("storage"))
    return (None if size == "5Gi"
            else f"ag-one-workspace must request the command's 5Gi; got {size}")
check("an agent command sizes its workspace at 5Gi", "agent",
      command_workspace_size)

AGENT_OBJECTS = sorted([
    "CiliumNetworkPolicy/ag-one-egress",
    "CiliumNetworkPolicy/ag-one-job-ingress",
    "CiliumNetworkPolicy/ag-one-toolset-ssh-egress",
    "ConfigMap/ag-one-toolset-secrets", "Deployment/ag-one",
    "PersistentVolumeClaim/ag-one-conversation",
    "PersistentVolumeClaim/ag-one-workspace",
    "Role/ag-one", "Role/ag-one-reach",
    "RoleBinding/ag-one",
    "Service/ag-one", "Service/ag-one-capability",
    "ServiceAccount/ag-one", "ServiceAccount/ag-one-unprivileged"])

def agent_references(docs):
    if objects(docs) != AGENT_OBJECTS:
        return f"want {AGENT_OBJECTS}, got {objects(docs)}"
    harness = pod(get(docs, "Deployment", "ag-one"))["spec"]
    rb = get(docs, "RoleBinding", "ag-one")
    got = (
        harness.get("serviceAccountName"),
        sorted(
            v["persistentVolumeClaim"]["claimName"]
            for v in harness.get("volumes") or []
            if "persistentVolumeClaim" in v
        ),
        rb["roleRef"]["name"], [s["name"]
                                for s in rb.get("subjects") or []],
        env_of(harness).get("CAPABILITY_SERVICE_NAME")
    )
    want = ("ag-one", ["ag-one-conversation"], "ag-one", ["ag-one"],
            "ag-one-capability")
    return None if got == want else f"want {want}, got {got}"
check(
    "an agent renders every object under its release name and references them "
    "by those names",
    "agent", agent_references)

def engine_caps(docs):
    d = get(docs, "Deployment", "inf-local")
    av = argv(d)
    got = {f: pair(av, f) for f in ("-m", "-c", "-np", "-t", "-n",
                                    "--sse-ping-interval")}
    want = {"-m": "/opt/m.gguf", "-c": "8192", "-np": "1", "-t": "2",
            "-n": "512", "--sse-ping-interval": "30"}
    limits = next(c for c in pod(d)["spec"]["containers"]
                  if c["name"] == "server")["resources"]["limits"]
    if (got != want or (limits.get("memory"), str(limits.get("cpu")))
        != ("4Gi", "2") or "--no-context-shift" not in av):
        return f"argv {av} limits {limits}"
check("engine caps follow the command, context shift off and SSE ping pinned",
      "local-override", engine_caps)

def thinking(_):
    got = {
        out: argv(get(docs_of(out), "Deployment", "inf-local"))
        for out in ("local", "local-think-false", "local-think-true")
    }
    want = {"local": [], "local-think-false": ["false"],
            "local-think-true": ["true"]}
    seen = {out: [v for f, v in zip(av, av[1:]) if f == "--reasoning"]
            for out, av in got.items()}
    return None if seen == want else f"want {want}, got {seen}"
check(
    "server.thinking renders --reasoning with its value, and no "
    "--reasoning when unset",
    None, thinking)

FETCH = {
    "curl", "wget", "oras", "crane", "skopeo", "docker", "git",
    "-hf", "--hf-repo", "-mu", "--model-url", "-dr", "--docker-repo"
}

def fetchers(spec):
    words = [
        w for c in spec.get("initContainers", []) + spec["containers"]
        for w in c.get("command", []) + c.get("args", [])
    ]
    return [w for w in words if w.rsplit("/", 1)[-1] in FETCH]

def copy_delivery(docs):
    d = get(docs, "Deployment", "inf-local")
    spec = pod(d)["spec"]
    inits = spec.get("initContainers") or []
    server = next(c for c in spec["containers"] if c["name"] == "server")
    vols = {v["name"]: v for v in spec["volumes"]}
    if (len(inits) != 1 or inits[0]["image"] != "ghcr.io/example/weights:1"
        or server["image"] != "llama-server:local"):
        return (
            "want one copy step from the weight image beside the runtime "
            f"image, got {inits} {server['image']}")
    cmd = inits[0]["command"]
    if (cmd[0].rsplit("/", 1)[-1] in ("sh", "bash", "ash") or "-c" in cmd
        or "/weights/model.gguf" not in cmd):
        return (
            f"copy step must be exec form reading the weights path, got {cmd}")
    scratch = [v for v in vols.values() if "emptyDir" in v]
    if (not scratch or any(
        not v["emptyDir"].get("sizeLimit")
        or v["emptyDir"].get("medium") == "Memory" for v in scratch
    )):
        return (
            f"copy target must be a size-limited disk emptyDir, got {scratch}")
    if any("image" in v for v in vols.values()):
        return "copy delivery carries an image volume"
    if (not all(m.get("readOnly") for m in server["volumeMounts"]
                if m["name"] == scratch[0]["name"])):
        return "server mounts the weight copy writable"
    return fetchers(spec) and f"fetches {fetchers(spec)}"
check("copy is the default weight delivery", "local", copy_delivery)

def mount_delivery(docs):
    spec = pod(get(docs, "Deployment", "inf-local"))["spec"]
    images = [v for v in spec["volumes"] if "image" in v]
    server = next(c for c in spec["containers"] if c["name"] == "server")
    if ([v["image"]["reference"] for v in images]
        != ["ghcr.io/example/weights:1"]):
        return f"want one image volume of the weight image, got {images}"
    if (spec.get("initContainers")
        or not any(
            m["name"] == images[0]["name"]
            for m in server["volumeMounts"]
        )):
        return (
            "want the server mounting the image volume and no copy step, got "
            f"{spec.get('initContainers')} {server['volumeMounts']}")
    return fetchers(spec) and f"fetches {fetchers(spec)}"
check("mount delivery uses an image volume and no copy step", "local-mount",
      mount_delivery)

def plumbing(out):
    docs = docs_of(out)
    spec = pod(get(docs, "Deployment", "inf-local"))["spec"]
    server = next(c for c in spec["containers"] if c["name"] == "server")
    av = argv(get(docs, "Deployment", "inf-local"))
    return (
        ((get(docs, "ConfigMap",
              "inf-local-chat-template") or {}).get("data") or {})
        .get("template"),
        [v for v in spec["volumes"] if v["name"] == "chat-template"],
        [m for m in server["volumeMounts"] if m["name"] == "chat-template"],
        pair(av, "--chat-template-file"),
    )

def chat_template(_):
    want = (
        "TEMPLATE-BODY\n",
        [{"name": "chat-template",
          "configMap": {"name": "inf-local-chat-template"}}],
        [{"name": "chat-template",
          "mountPath": "/etc/sycophant/chat-template/template",
          "subPath": "template", "readOnly": True}],
        "/etc/sycophant/chat-template/template",
    )
    got = {o: plumbing(o) for o in ("local-tpl-copy",
                                    "local-tpl-mount", "local")}
    if (got != {"local-tpl-copy": want, "local-tpl-mount": want,
                "local": (None, [], [], None)}):
        return f"want {want} on both deliveries and nothing unset, got {got}"
check(
    "chat template renders all four parts on both deliveries, none when unset",
    None, chat_template)

def placement(out):
    spec = pod(get(docs_of(out), "Deployment", "inf-local"))["spec"]
    return spec.get("nodeSelector"), spec.get("tolerations")

def node_separation(_):
    default = placement("local")
    if default[0] != {"sycophant.md/workload": "inference"} \
            or placement("local-single-false") != default or placement(
                "local-single-true") != (None, None):
        return (
            f"default {default}, false {placement('local-single-false')}, true "
            f"{placement('local-single-true')}")
    sched = (yaml.safe_load(get(
        docs_of("bare"), "ConfigMap",
        "toolset-scheduling"
    )["data"]["scheduling.yaml"]))
    values = [t.get("value") for t in sched["tolerations"]]
    return values != ["toolset"] and f"toolset pods tolerate {values}"
check(
    "inference and toolset pods stay on separate nodes unless the "
    "exception is declared",
    None, node_separation)

def headless_service(docs):
    for d in docs:
        spec = d.get("spec") or {}
        sel = spec.get("selector") or {}
        if d["kind"] == "Service" and spec.get("clusterIP") == "None" \
                and sel.get("app.kubernetes.io/component") == "capability-job" \
                and sel.get("sycophant.md/agent") == "one" \
                and any(str(p.get("port")) == "9090"
                        for p in spec.get("ports") or []):
            return None
    return "no headless Service selects agent one's capability-job pods on 9090"
check("a headless Service addresses each agent's capability-job pods on 9090",
      "agbase", headless_service)

def tool_pod_ingress(docs):
    harness = {
        "app.kubernetes.io/component": "harness",
        "sycophant.md/agent": "one"
    }
    for c in cnps(docs):
        sel = (c["spec"].get("endpointSelector") or {}).get("matchLabels") or {}
        if ((
            sel.get("app.kubernetes.io/component"),
            sel.get("sycophant.md/agent")
        ) != ("capability-job", "one")):
            continue
        for rule in c["spec"].get("ingress") or []:
            if (9090 in ports(rule) and any(
                selects(harness,
                        e.get("matchLabels") or {})
                for e in rule.get("fromEndpoints") or []
            )):
                return None
    return (
        "no policy admits agent one's harness to its capability-job "
        "pods on 9090")
check("capability-job pods admit their own agent's harness on 9090", "agbase",
      tool_pod_ingress)

def reach_role(docs):
    role = get(docs, "Role", "ag-one-reach")
    if role is None:
        return (
            "no Role/ag-one-reach in the render; the agent chart must render "
            "the reach Role")
    if role["metadata"].get("namespace") != "tenant-acme":
        return (
            "Role namespace must be the release namespace, got "
            f"{role['metadata']}")
    want = [{
        "apiGroups": [""], "resources": ["services"], "verbs": ["get"],
        "resourceNames": ["ag-one"]
    }]
    return (None if role.get("rules") == want
            else f"the reach Role's rules must be exactly {want}, got "
            f"{role.get('rules')}")
check(
    "the agent chart renders Role ag-<name>-reach granting get on exactly "
    "Service ag-<name>",
    "agbase", reach_role)

def refuses_missing_agent():
    name = (
        "a user command naming an agent that does not exist fails the install "
        "and renders nothing")
    docs, err = load("user-missing-ag")
    if docs is not None:
        bad(name,
            "the user chart rendered a grant to agent alpha, which "
            "does not exist")
    elif "agent alpha does not exist" not in err:
        bad(name,
            "the render failed, but not on the missing agent: "
            f"{err[-300:]}")
    else:
        ok(name)
refuses_missing_agent()

def grants_no_verb(docs):
    verb_kinds = {d["kind"] for d in docs} & {"Role", "ClusterRole"}
    return (verb_kinds
            and f"the user chart renders {sorted(verb_kinds)}; it must grant "
            "no verb and no ConfigMap access")
check(
    "the user chart renders no Role and no ClusterRole, so a user command "
    "grants no verb",
    "user", grants_no_verb)

def identity_cm(docs):
    cm = get(docs, "ConfigMap", f"user-{hash10}")
    if cm is None:
        return (
            f"no ConfigMap/user-{hash10} in the render; the identity "
            "ConfigMap is missing")
    labels = cm["metadata"].get("labels") or {}
    if labels.get("sycophant.md/type") != "relay-identity":
        return (
            "the identity ConfigMap must carry sycophant.md/type: "
            f"relay-identity, got {labels}")
    if labels.get("app.kubernetes.io/part-of") != "sycophant":
        return (
            "the identity ConfigMap must carry the sycophant labels, "
            f"got {labels}")
    want = {"subject": subject, "handle.slack": "U12345"}
    return (None if cm.get("data") == want
            else "the identity ConfigMap must carry the command data "
            f"verbatim, want {want}, got {cm.get('data')}")
check("the identity ConfigMap carries the subject and the handle verbatim",
      "user", identity_cm)

def membership_binding(docs):
    rb = get(docs, "RoleBinding", f"user-{hash10}")
    if rb is None:
        return (
            f"no RoleBinding/user-{hash10} in the render; provision-identity "
            "membership must survive")
    subjects = [(s.get("kind"), s.get("name"))
                for s in rb.get("subjects") or []]
    if subjects != [("User", subject)]:
        return f"membership binding must carry the raw subject, got {subjects}"
    ref = rb["roleRef"]
    if ((ref.get("kind"), ref.get("name")) != (
        "ClusterRole",
        "provision-identity"
    )):
        return (
            f"membership binding must reference provision-identity, got {ref}")
check("the provision-identity membership RoleBinding renders for the subject",
      "user", membership_binding)

def tenant_ns(docs):
    labels = get(docs, "Namespace", "tenant-acme")["metadata"]["labels"]
    want = {"app.kubernetes.io/part-of": "sycophant-tenant",
            "pod-security.kubernetes.io/enforce": "restricted",
            "pod-security.kubernetes.io/enforce-version": "latest",
            "pod-security.kubernetes.io/warn": "restricted",
            "pod-security.kubernetes.io/audit": "restricted"}
    got = {k: labels.get(k) for k in want}
    return None if got == want else f"want {want}, got {got}"
check(
    "the tenant chart renders the namespace with the perimeter and "
    "restricted PSA labels",
    "ns", tenant_ns)

def agent_identities(n, parent=None):
    def fn(docs):
        sas = {
            d["metadata"]["name"]: d for d in docs
            if d["kind"] == "ServiceAccount"
        }
        if (any((d["metadata"].get("labels") or {}).get("sycophant.md/type")
                != "agent-sa" for d in sas.values())):
            return ("every agent ServiceAccount must carry "
                    "sycophant.md/type: agent-sa")
        if (sas[f"ag-{n}-unprivileged"].get("automountServiceAccountToken")
            is not False):
            return f"ag-{n}-unprivileged must not automount a token"
        labels = sas[f"ag-{n}"]["metadata"].get("labels") or {}
        got = labels.get("sycophant.md/parent")
        if got != parent:
            return f"ag-{n} sycophant.md/parent must be {parent}, got {got}"
        rules = sorted(
            (
                tuple(r.get("apiGroups") or []),
                tuple(r.get("resources") or []),
                tuple(sorted(r.get("verbs") or []))
            )
            for r in get(docs, "Role", f"ag-{n}").get("rules") or []
        )
        want = (sorted([(("batch",), ("jobs",), ("create",)),
                        (("",), ("configmaps",), ("list", "watch"))]))
        return (None if rules == want
                else f"harness Role must be exactly {want}, got {rules}")
    return fn
check(
    "agent ServiceAccounts are labelled, the unprivileged one mounts no token, "
    "ag-one names no parent, and the harness Role is exactly: create Jobs, "
    "list and watch ConfigMaps",
    "agent", agent_identities("one"))
check(
    "a sub-agent's ServiceAccounts are labelled, the unprivileged one mounts "
    "no token, ag-sub names its parent, and its Role is the agent's Role: "
    "create Jobs, list and watch ConfigMaps, no delete",
    "subagent", agent_identities("sub", "one"))

SUBAGENT_OBJECTS = sorted([
    "CiliumNetworkPolicy/ag-sub-egress",
    "CiliumNetworkPolicy/ag-sub-job-ingress",
    "CiliumNetworkPolicy/ag-sub-toolset-ssh-egress",
    "ConfigMap/ag-sub-harness",
    "ConfigMap/ag-sub-toolset-secrets", "Role/ag-sub", "RoleBinding/ag-sub",
    "Service/ag-sub-capability",
    "ServiceAccount/ag-sub", "ServiceAccount/ag-sub-unprivileged"])

def subagent_objects(docs):
    if objects(docs) != SUBAGENT_OBJECTS:
        return f"want {SUBAGENT_OBJECTS}, got {objects(docs)}"
    secrets = get(docs, "ConfigMap", "ag-sub-toolset-secrets").get("data")
    if secrets != {"sub-key": "ssh"}:
        return f"want only the sub-agent's own Secret, got {secrets}"
    cnps = [d for d in docs if d["kind"] == "CiliumNetworkPolicy"]
    key = "sycophant.md/agent"
    owners = [(d["metadata"].get("labels") or {}).get(key) for d in cnps]
    if any(o != "sub" for o in owners):
        return "every CNP must be keyed to the sub-agent"
    eg = get(docs, "CiliumNetworkPolicy", "ag-sub-toolset-ssh-egress")
    rules = eg["spec"]["egress"]
    names = [f["matchName"] for r in rules for f in r.get("toFQDNs") or []]
    fqdns = sorted(names)
    return (None if fqdns == ["example.com"]
            else f"want only the sub-agent's egress example.com, got {fqdns}")
check(
    "a sub-agent renders no Deployment, relay Service, PVC or reach Role, "
    "and its Secrets and egress come from its own command",
    "subagent", subagent_objects)

def subagent_instructions_source(docs):
    cm = get(docs, "ConfigMap", "ag-sub-harness")
    if cm is None:
        return "no ConfigMap/ag-sub-harness in the render"
    src = (cm.get("data") or {}).get("INSTRUCTIONS_SOURCE")
    want = ("os:sycophant-instructions/system:serviceaccount:"
            "tenant-acme:ag-sub/")
    return (None if src == want
            else f"INSTRUCTIONS_SOURCE must be {want!r}, got {src!r}")
check(
    "a sub-agent's harness ConfigMap names the sub-agent's own instructions "
    "folder",
    "subagent", subagent_instructions_source)

def subagent_ingress(docs):
    rules = [
        r for p in cnps(docs)
        if ((p["spec"].get("endpointSelector") or {}).get("matchLabels")
            or {}).get("app.kubernetes.io/component") == "harness"
        for r in p["spec"].get("ingress") or []
    ]
    got = [(sorted(r), [e.get("matchLabels")
                        for e in r.get("fromEndpoints") or []],
            r.get("toPorts")) for r in rules]
    want = [(
        ["fromEndpoints", "toPorts"],
        [{
            "app.kubernetes.io/component": "harness",
            "app.kubernetes.io/part-of": "sycophant",
            "sycophant.md/agent": "one",
        }],
        [{"ports": [{"port": "9092", "protocol": "TCP"}]}],
    )]
    return None if got == want else f"want {want}, got {got}"
check(
    "a sub-agent's harness pods admit only the harness pods of its parent "
    "agent, on 9092 only",
    "subagent", subagent_ingress)

# Every Role bound to relay-controller, in the tenant and channel charts: the
# identity table (ConfigMaps) is read-only, and no
# workload or Secret is reachable.
def relay_roles(docs):
    names = [
        rb["roleRef"]["name"] for rb in docs if rb["kind"] == "RoleBinding"
        and any(s.get("name") == "relay-controller"
                for s in rb.get("subjects") or [])
    ]
    rules = [
        r for d in docs if d["kind"] == "Role"
        and d["metadata"]["name"] in names for r in d.get("rules") or []
    ]
    cm = (set().union(*[set(r.get("verbs") or []) for r in rules
                        if "configmaps" in (r.get("resources") or [])]))
    reach = {res for r in rules for res in r.get("resources") or []} & {
        "pods",
        "jobs", "deployments", "replicasets",
        "statefulsets", "daemonsets", "secrets", "*"
    }
    if not names or cm != {"get", "list", "watch"} or reach:
        return (
            f"roles {names}: ConfigMap verbs {sorted(cm)} (want "
            f"get/list/watch), forbidden resources {sorted(reach)}")
check("the relay reads ConfigMaps only and reaches no workload or Secret",
      None, lambda _: relay_roles(docs_of("bare") + docs_of("channel")))

def floor(docs):
    spec = get(docs, "CiliumNetworkPolicy", "default-deny-egress")["spec"]
    got = (
        spec.get("endpointSelector"), spec.get("enableDefaultDeny"),
        spec.get("egress")
    )
    return (None if got == ({}, {"egress": True}, [{}])
            else "want an empty selector, egress default-deny and one empty "
            f"rule, got {got}")
check("the namespace egress floor selects every pod and allows nothing",
      "bare", floor)

def install_wait_egress(docs):
    spec = get(docs, "CiliumNetworkPolicy", "install-wait-egress")["spec"]
    want = ({
        "app.kubernetes.io/component": "install-wait",
        "app.kubernetes.io/part-of": "sycophant"
    },
            [{"toEntities": ["kube-apiserver"],
              "toPorts": [{"ports": [{"port": "6443", "protocol": "TCP"}]}]}])
    got = (spec["endpointSelector"].get("matchLabels"), spec.get("egress"))
    return None if got == want else f"want {want}, got {got}"
check("install-wait reaches only the apiserver on 6443", "bare",
      install_wait_egress)

# Cilium unions same-port L7 DNS rules across policies on a pod: one L4-only
# kube-dns rule reopens DNS tunnelling for every sibling policy of the
# agent and model pods.
def kube_dns_l7(_):
    outs = [o[:-5] for o in sorted(os.listdir(work)) if o.endswith(".yaml")
            and o.startswith(("agent", "agbase", "local", "remote"))]
    bare_dns = [(o, p["metadata"]["name"]) for o in outs
                for p in cnps(docs_of(o))
                for r in p["spec"].get("egress") or []
                if any(
                    (e.get("matchLabels") or {}).get("k8s-app") == "kube-dns"
                    for e in r.get("toEndpoints") or []
                )
                for t in r.get("toPorts") or []
                if "dns" not in (t.get("rules") or {})]
    egress = get(
        docs_of("agent"), "CiliumNetworkPolicy",
        "ag-one-toolset-ssh-egress"
    )["spec"]["egress"]
    names = sorted(
        (
            n for r in egress for t in r.get("toPorts") or []
            for n in (t.get("rules") or {}).get("dns") or []
        ),
        key=str
    )
    wide = [r for r in egress if r.get("toEntities") or r.get("toCIDR")]
    return (bare_dns or names != [{"matchName": "github.com"},
                                  {"matchName": "gitlab.com"}] or wide) and \
        (
            f"kube-dns rules without L7 dns {bare_dns}; toolset dns {names}; "
            f"toolset entity/CIDR rules {wide}"
        )
check(
    "every agent and model kube-dns rule carries an L7 allowlist; a toolset "
    "resolves only its own names",
    None, kube_dns_l7)

def model_egress(_):
    def shape(out, key):
        p = get(docs_of(out), "CiliumNetworkPolicy", f"inf-{key}")["spec"]
        eg = p.get("egress") or []
        return (p["endpointSelector"].get("matchLabels"),
                sorted(
                    n.get("matchName") or "pattern" for r in eg
                    for t in r.get("toPorts") or []
                    for n in (t.get("rules") or {}).get("dns") or []
                ),
                [
                    (
                        sorted(r.get("toFQDNs") or [], key=str),
                        r.get("toCIDR"), r.get("toEntities"), r.get("toPorts")
                    )
                    for r in eg if not r.get("toEndpoints")
                ])
    sel = lambda key: {
        "sycophant.md/model": key,
        "sycophant.md/job-kind": "inference",
        "app.kubernetes.io/part-of": "sycophant"
    }
    tcp = lambda port: [{"ports": [{"port": port, "protocol": "TCP"}]}]
    want = {"remote": (
        sel("deepseek"), ["openrouter.ai"],
        [([{"matchName": "openrouter.ai"}], None, None, tcp("443"))]
    ),
            "remote-ip": (sel("ipdirect"), [],
                          [([], ["10.42.0.9/32"], None, tcp("8080"))])}
    got = {
        "remote": shape("remote", "deepseek"),
        "remote-ip": shape("remote-ip", "ipdirect")
    }
    return None if got == want else f"want {want}, got {got}"
check(
    "a remote model's one egress hole derives from baseUrl: FQDN plus L7 DNS "
    "for a host, a /32 and no DNS for an IP",
    None, model_egress)

def inference_pod(docs):
    spec = pod(get(docs, "Deployment", "inf-local"))["spec"]
    conts = (spec.get("initContainers") or []) + spec["containers"]
    sc = lambda c: c.get("securityContext") or {}
    problems = [
        spec.get("automountServiceAccountToken") is not False
        and "automountServiceAccountToken",
        (spec.get("securityContext") or {}).get("runAsNonRoot") is not True
        and "runAsNonRoot",
        (spec.get("securityContext") or {}).get("seccompProfile", {})
        .get("type") != "RuntimeDefault" and "seccompProfile",
        spec.get("runtimeClassName") and "runtimeClassName",
        any(spec.get(h) for h in ("hostNetwork", "hostPID", "hostIPC"))
        and "host namespace",
        spec.get("serviceAccountName") not in (None, "default")
        and "serviceAccountName",
        any("serviceAccountToken" in s for v in spec.get("volumes") or []
            for s in (v.get("projected") or {}).get("sources") or [])
        and "projected token",
        any("hostPath" in v for v in spec.get("volumes") or []) and "hostPath",
        any(p.get("hostPort") for c in conts for p in c.get("ports") or [])
        and "hostPort",
        any((
            sc(c).get("readOnlyRootFilesystem"),
            sc(c).get("allowPrivilegeEscalation")
        ) != (True, False)
            or "ALL" not in (sc(c).get("capabilities") or {}).get("drop", [])
            for c in conts) and "container floor",
        any((sc(c).get("capabilities") or {}).get("add") for c in conts)
        and "capabilities.add",
    ]
    return [p for p in problems if p] or None
check(
    "the local model pod is hardened: no token, no host reach, no extra "
    "runtime or capability",
    "local", inference_pod)

def inference_fence(docs):
    spec = get(docs, "CiliumNetworkPolicy", "inf-local")["spec"]
    ing, eg = spec.get("ingress") or [], spec.get("egress") or []
    routes = sorted((h.get("method"), h.get("path")) for r in ing
                    for t in r.get("toPorts") or []
                    for h in (t.get("rules") or {}).get("http") or [])
    got = (
        [
            [e.get("matchLabels") for e in r.get("fromEndpoints") or []]
            for r in ing
        ],
        [
            p for r in ing for t in r.get("toPorts") or []
            for p in t.get("ports") or []
        ], routes,
        [[(e.get("matchLabels") or {}).get("k8s-app")
          for e in r.get("toEndpoints") or []] + sorted(
              k for k in r
              if k not in ("toEndpoints", "toPorts")
          ) for r in eg]
    )
    want = ([[{
        "app.kubernetes.io/component": "harness",
        "app.kubernetes.io/part-of": "sycophant"
    }]],
            [{"port": "8080", "protocol": "TCP"}],
            [("POST", "/v1/chat/completions"), ("POST", "/v1/completions")],
            [["kube-dns"]])
    return None if got == want else f"want {want}, got {got}"
check(
    "the local model admits only harness on 8080 for two POST routes and "
    "egresses only to kube-dns",
    "local", inference_fence)

def store_ingress(docs):
    d = get(docs, "CiliumNetworkPolicy", "objectstore")
    rules = (d or {}).get("spec", {}).get("ingress") or []
    got = sorted((
        json.dumps([fe.get("matchLabels")
                    for fe in r.get("fromEndpoints") or []], sort_keys=True),
        json.dumps([fe.get("matchExpressions")
                    for fe in r.get("fromEndpoints") or []], sort_keys=True),
        sorted(ports(r))
    ) for r in rules)
    harness = (
        json.dumps([{
            "app.kubernetes.io/component": "harness",
            "app.kubernetes.io/part-of": "sycophant"
        }], sort_keys=True),
        json.dumps([[{"key": "k8s:io.kubernetes.pod.namespace",
                      "operator": "Exists"}]], sort_keys=True), [8333]
    )
    filer = [r for r in rules if ports(r) == {8888}]
    # A fromEndpoints selector with no namespace label
    # matches the policy's own namespace.
    filer_from = sorted((
        (fe.get("matchLabels") or {})
        .get("app.kubernetes.io/component"),
        (fe.get("matchLabels") or {}).get("k8s:io.kubernetes.pod.namespace",
                                          "sycophant-system")
    )
                        for r in filer for fe in r.get("fromEndpoints") or [])
    exprs = [
        fe.get("matchExpressions") for r in filer
        for fe in r.get("fromEndpoints") or []
        if fe.get("matchExpressions")
    ]
    problems = [
        harness not in got
        and f"harness must reach 8333 from every namespace, got {got}",
        len(rules) != 2
        and "want exactly the harness 8333 rule and one filer 8888 "
        f"rule, got {got}",
        filer_from != [
            ("provision-instructions", "sycophant-system"),
            ("reap-tenant-files", "sycophant-system"),
            ("uninstall-instructions", "sycophant-system")
        ]
        and "filer 8888 must admit only the instructions and "
        f"reap-tenant-files job pods in sycophant-system, got {filer_from}",
        exprs
        and f"the filer rule must not widen past sycophant-system: {exprs}",
    ]
    return [p for p in problems if p] or None
check(
    "the object store admits harness pods on 8333 and only the instructions "
    "jobs on the filer's 8888",
    "store", store_ingress)

def store_filer_port(docs):
    svc = get(docs, "Service", "seaweedfs")
    got = sorted(
        (p.get("name"), p.get("port"))
        for p in (svc or {}).get("spec", {}).get("ports") or []
    )
    return (None if got == [("filer", 8888), ("s3", 8333)]
            else "Service seaweedfs ports must be s3 8333 and filer "
            f"8888, got {got}")
check("the store Service publishes the filer on 8888 beside s3", "store",
      store_filer_port)

def store_egress(docs):
    p = [
        d for d in cnps(docs)
        if (d["spec"].get("endpointSelector") or {}).get("matchLabels", {})
        .get("app.kubernetes.io/component") == "objectstorage"
    ]
    if len(p) != 1:
        return f"{len(p)} store policies"
    eg = p[0]["spec"].get("egress") or []
    peers = [
        e.get("matchLabels") for r in eg for e in r.get("toEndpoints")
        or [] if (e.get("matchLabels") or {}).get("k8s-app") != "kube-dns"
    ]
    dns = sorted(
        json.dumps(n, sort_keys=True) for r in eg
        for t in r.get("toPorts") or []
        for n in (t.get("rules") or {}).get("dns") or []
    )
    got = (
        peers, [f.get("matchName") for r in eg
                for f in r.get("toFQDNs") or []],
        [c for r in eg for c in r.get("toCIDR") or []],
        [r for r in eg if r.get("toEntities")], dns
    )
    want = ([], [], [], [{
        "toEntities": ["kube-apiserver"],
        "toPorts": [{"ports": [{"port": "6443", "protocol": "TCP"}]}]
    }],
            [json.dumps({"matchName": "kubernetes.default.svc.cluster.local"})])
    return None if got == want else f"want {want}, got {got}"
check("the store egresses only to DNS and the apiserver", "store", store_egress)

def store_pod(docs):
    spec = (pod(next(
        d for d in docs if d["kind"] == "Deployment"
        and pod(d)["metadata"]["labels"].get("app.kubernetes.io/component")
        == "objectstorage"
    ))["spec"])
    c, = spec["containers"]
    args = " ".join((c.get("args") or []) + (c.get("command") or []))
    csc, psc = c.get("securityContext") or {}, spec.get("securityContext") or {}
    problems = [
        "-s3.iam.config" not in args and "-s3.iam.config",
        (psc.get("runAsNonRoot"), psc.get("fsGroup")) != (True, 1000)
        and "pod securityContext",
        spec.get("automountServiceAccountToken") is not False
        and "automountServiceAccountToken",
        (
            csc.get("allowPrivilegeEscalation"),
            csc.get("readOnlyRootFilesystem"),
            (csc.get("capabilities") or {}).get("drop")
        ) != (False, True, ["ALL"])
        and "container securityContext",
    ]
    return [p for p in problems if p] or None
check(
    "the store pod runs SeaweedFS with its IAM config, no token and a "
    "read-only root",
    "store", store_pod)

def sync_token_only(docs):
    spec = (pod(next(
        d for d in docs if d["kind"] == "Deployment"
        and d["metadata"]["labels"].get("app.kubernetes.io/component")
        == "harness"
    ))["spec"])
    blob = json.dumps(spec)
    holders = [
        c["name"]
        for c in (spec.get("initContainers") or []) + spec["containers"]
        if any(
            m["name"] == "objectstore-token"
            for m in c.get("volumeMounts") or []
        )
    ]
    if holders != ["instructions-sync"]:
        return (
            "objectstore-token must mount in instructions-sync only, "
            f"got {holders}")
    if ("secretKeyRef" in blob or "AWS_ACCESS_KEY_ID" in blob
        or any("secret" in v for v in spec.get("volumes") or [])):
        return "the harness pod must hold no static store credential"
    hostpaths = [
        v["name"] for d in docs if d["kind"] in ("Deployment", "Job")
        for v in pod(d)["spec"].get("volumes") or [] if "hostPath" in v
    ]
    return hostpaths and f"hostPath volumes rendered: {hostpaths}"
check(
    "only instructions-sync holds the store token; no static keys "
    "and no hostPath",
    "agbase", sync_token_only)

def channel_needs_gvisor(_):
    path = f"{work}/channel-hardening.json"
    if not os.path.exists(path):
        return (
            "no kyverno case: want Deployment ch-tailnet in the tailnet "
            "channel render and cluster-gvisor-pod-policy in the "
            "cluster render")
    rows = {r.get("RESOURCE", "").rsplit("/", 1)[-1]: r
            for r in json.load(open(path))}
    as_rendered, stripped = (
        rows.get("ch-tailnet-as-rendered"),
        rows.get("ch-tailnet-no-runtime-class")
    )
    problems = [
        (not as_rendered or as_rendered.get("RESULT") != "Pass")
        and f"the rendered ch-tailnet pod must be admitted: {as_rendered}",
        (
            not stripped or stripped.get("RESULT") != "Pass"
            or "gvisor" not in (stripped.get("Message") or "")
        )
        and "ch-tailnet without runtimeClassName must be refused naming "
        f"gvisor: {stripped}",
    ]
    return [p for p in problems if p] or None
check(
    "the pod-hardening VAP refuses the tailnet channel pod without "
    "runtimeClassName",
    None, channel_needs_gvisor)

def channel_identity_secret(_):
    channel = docs_of("channel-tailnet")
    cluster = docs_of("cluster")
    def names(rules):
        return (sorted({n for r in rules or []
                        for n in r.get("resourceNames") or []}))
    dep = get(channel, "Deployment", "ch-tailnet")
    env = env_of(pod(dep)["spec"]) if dep else {}
    roles = [
        d for d in channel if d["kind"] == "Role"
        and names(d.get("rules"))
    ]
    allow = [
        d for d in cluster if d["kind"] == "ValidatingAdmissionPolicy"
        and d["metadata"]["name"].endswith("secret-name-allowlist")
    ]
    exprs = " ".join(
        v.get("expression", "") for d in allow
        for v in d["spec"].get("validations") or []
    )
    places = {
        "channel Role resourceNames": [
            n for d in roles
            for n in names(d["rules"])
        ],
        "TS_KUBE_SECRET": [env.get("TS_KUBE_SECRET")],
        "Secret-name allowlist VAP": re
        .findall(r"object\.metadata\.name == '([^']*)'", exprs),
        "provision-write ClusterRole": names((get(
            cluster, "ClusterRole",
            "provision-write"
        ) or {}).get("rules")),
        "Kyverno controller RBAC": names((get(
            cluster, "ClusterRole",
            "kyverno:sycophant:provision"
        ) or {}).get("rules")),
    }
    wrong = {k: v for k, v in places.items() if "tailnet-identity" not in v}
    return wrong and f"want tailnet-identity, got {wrong}"
check("the channel identity Secret is tailnet-identity in all five places",
      None, channel_identity_secret)

# The dispatch checks the parent's token against HARNESS_DISPATCH_AUDIENCE, so
# the token the agent's harness mounts must be minted for that audience.
def dispatch_token_audience(docs):
    src = open(os.path.join(root, "crates/shared/src/auth.rs")).read()
    want = re.search(r'HARNESS_DISPATCH_AUDIENCE: &str = "([^"]*)"', src)[1]
    spec = pod(get(docs, "Deployment", "ag-one"))["spec"]
    vol = [v for v in spec["volumes"] if v["name"] == "harness-dispatch-auth"]
    srcs = [s for v in vol for s in v["projected"]["sources"]]
    got = [s["serviceAccountToken"]["audience"] for s in srcs]
    return got != [want] and f"want [{want}], got {got}"
check("an agent's harness-dispatch-auth token has the code's dispatch audience",
      "agbase", dispatch_token_audience)

print(f"\n{passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
PY
