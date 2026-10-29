#!/usr/bin/env bash
# Offline: renders the command VAPs from the cluster chart and runs each case
# directory through `kyverno test`, then checks that every rejection names its
# field and that every template in the commands chart, rendered with the test
# values, is admitted. No cluster.
#
#   bash tests/unit/command-vap/acceptance.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE
source "$HERE/../lib.sh"
cp -R "$HERE/." "$WORK/"

helm template c "$ROOT/charts/sycophant-cluster" -n kyverno \
  --api-versions policies.kyverno.io/v1/ValidatingPolicy \
  --show-only templates/command-vap.yaml > "$WORK/policy.yaml" || exit 1
helm template c "$ROOT/charts/sycophant-cluster" -n kyverno \
  --api-versions policies.kyverno.io/v1/ValidatingPolicy \
  --set selfService.enabled=true \
  --set-string selfService.operator=https://sycophant-dex.sycophant-system.svc.cluster.local:5556/dex#operator \
  --show-only templates/command-vap.yaml > "$WORK/policy-self-service.yaml" \
  || bad "command VAPs render with the self-service setting on" "helm template failed"

python3 - "$WORK" "$HERE" "$ROOT/charts/sycophant-commands" <<'PY'
import os, subprocess, sys, yaml
work, here, chart = sys.argv[1:4]
case = os.path.join(work, "commands")
os.makedirs(case)
checks = []
def check(name, good, detail=""):
    checks.append(f"{'ok' if good else 'bad'}\t{name}\t{detail}\n")

tdir = os.path.join(chart, "templates")
templates = sorted(f[:-len(".yaml")] for f in os.listdir(tdir) if f.endswith(".yaml")) if os.path.isdir(tdir) else []

renders = [(t, "command-values.yaml", t) for t in templates]
renders.append(("inference-provision", "command-values-remote.yaml", "inference-provision-remote"))
rendered = {}
for template, values, out in renders:
    p = subprocess.run(["helm", "template", "c", chart, "-f", os.path.join(here, values),
                        "--show-only", f"templates/{template}.yaml"], capture_output=True, text=True)
    if p.returncode != 0:
        check(f"render {out}", False, p.stderr.strip().splitlines()[-1] if p.stderr.strip() else "")
        continue
    docs = [d for d in yaml.safe_load_all(p.stdout) if d]
    if len(docs) != 1 or docs[0].get("kind") != "ConfigMap":
        check(f"render {out}", False, f"{len(docs)} objects, not one ConfigMap")
        continue
    rendered[out] = docs[0]
    yaml.safe_dump(docs[0], open(os.path.join(case, out + ".yaml"), "w"))

def summary(out):
    d = rendered.get(out)
    return d and (d["metadata"]["name"], d["metadata"]["namespace"], d.get("data") or {})

want = ("provision-workspace-hello-world", "tenant-acme", {
    "toolsets": "hello-world",
    "models": "deepseek,local",
    "storage": "5Gi",
    "grant.hello-world.demo-key.secret": "demo-ssh-key",
    "grant.hello-world.demo-key.path": "/home/agent/.ssh/id_ed25519",
    "grant.hello-world.demo-key.egress": "git.example.com",
})
check("workspace values flatten to command data", summary("workspace-provision") == want,
      f"got: {summary('workspace-provision')}")

want = ("provision-tenant-acme", "sycophant-system", {"tenant": "acme", "capacity": "2Gi", "owner": "https://sycophant-dex.sycophant-system.svc.cluster.local:5556/dex#operator", "owner-hash": "027164b38d531173", "headscale.enabled": "true", "headscale.serverUrl": "http://localhost:8080", "headscale.magicDns": "true", "headscale.baseDomain": "ts.local", "headscale.acme.enabled": "false", "headscale.acme.email": "ops@example.com"})
check("tenant values flatten to command data", summary("tenant-provision") == want,
      f"got: {summary('tenant-provision')}")

want = ("provision-toolset-hello-world", "tenant-acme", {
    "image": "ghcr.io/sycophant-md/hello-world@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    "keepalive": "true",
    "deadlineSeconds": "300",
    "env.GIT_TERMINAL_PROMPT": "0",
    "tool.git-status.description": "Show the working tree status in the workspace.",
    "tool.git-clone.description": "Clone a git repository.",
    "tool.git-clone.arg.url.type": "string",
    "tool.git-clone.arg.url.required": "true",
    "tool.git-clone.arg.url.env": "URL",
    "tool.git-clone.arg.url.description": "Repository URL.",
})
check("toolset values flatten to command data", summary("toolset-provision") == want,
      f"got: {summary('toolset-provision')}")

want = ("provision-user-b212e3ad41", "tenant-acme", {"user": "https://dex.example.com/dex#alice"})
check("user command named by the subject's sha256 handle", summary("user-provision") == want,
      f"got: {summary('user-provision')}")

want = ("provision-channel-app", "tenant-acme", {
    "storageClass": "local-path",
    "image": "tailscale/tailscale",
    "tag": "v1.80.0",
    "pullPolicy": "Always",
    "loginServer": "http://headscale:8080",
    "hostname": "relay",
    "authKeySecret": "relay-tsnet-authkey",
})
check("channel values flatten to command data", summary("channel-provision") == want,
      f"got: {summary('channel-provision')}")

archive = yaml.safe_load(open(os.path.join(here, "command-values.yaml")))["instructions"]["archive"]
want = ("provision-instructions-hello-world", "tenant-acme", {"archive": archive})
check("instructions values render to a command carrying only the archive", summary("instructions-provision") == want,
      f"got: {summary('instructions-provision')}")
want = ("reap-instructions-hello-world", "tenant-acme", {})
check("an instructions reap renders reap-instructions-<workspace> with no data", summary("instructions-reap") == want,
      f"got: {summary('instructions-reap')}")

results, names = [], []
for out, d in sorted(rendered.items()):
    kind, verb = out.split("-")[:2]
    m = d["metadata"]
    names.append(m["name"])
    results.append({"policy": f"command-{verb}-{kind}", "isValidatingAdmissionPolicy": True,
                    "kind": "ConfigMap", "result": "pass",
                    "resources": [f"{m['namespace']}/{m['name']}"]})
open(os.path.join(work, "admitted"), "w").write("".join(n + "\n" for n in names))
open(os.path.join(work, "checks"), "w").write("".join(checks))
yaml.safe_dump({"apiVersion": "cli.kyverno.io/v1alpha1", "kind": "Test",
                "metadata": {"name": "command-vap-commands"}, "policies": ["../policy.yaml"],
                "resources": [o + ".yaml" for o in sorted(rendered)], "userinfo": "../operator.yaml",
                "variables": "../values.yaml", "results": results},
               open(os.path.join(case, "kyverno-test.yaml"), "w"))
PY

# The walkthrough: the commands chart rendered from the example values files,
# the way `sycoadm <kind> up -f` renders them. The toolset render carries a
# digest-pinned image and its tools.yaml as `toolset.tools`, as the image push
# and its md.sycophant.tools label supply them; the command VAP must admit it.
python3 - "$WORK" "$ROOT" <<'PY'
import os, re, subprocess, sys, yaml
work, root = sys.argv[1:3]
chart = os.path.join(root, "charts", "sycophant-commands")
ex = os.path.join(root, "examples", "sycoadm")
checks = []
def check(name, good, detail=""):
    checks.append(f"{'ok' if good else 'bad'}\t{name}\t{detail}\n")

def load(path):
    return yaml.safe_load(open(path)) if os.path.isfile(path) else None

hw_tools_path = os.path.join(ex, "toolsets", "hello-world", "tools.yaml")
hw_tools = load(hw_tools_path)
base_tools = load(os.path.join(root, "images", "toolset", "tools.yaml")) or {}

def render(template, values, extra=()):
    if not os.path.isfile(values):
        return None, f"missing {os.path.relpath(values, root)}"
    p = subprocess.run(["helm", "template", "c", chart, "-f", values, *extra,
                        "--show-only", f"templates/{template}.yaml"], capture_output=True, text=True)
    if p.returncode != 0:
        return None, (p.stderr.strip().splitlines() or [""])[-1]
    docs = [d for d in yaml.safe_load_all(p.stdout) if d]
    if len(docs) != 1 or docs[0].get("kind") != "ConfigMap":
        return None, f"{len(docs)} objects, not one ConfigMap"
    return docs[0], ""

toolset_extra = ["--set-string", "toolset.image=registry.local/toolset-hello-world@sha256:" + "0" * 64]
if isinstance(hw_tools, dict):
    tools_values = os.path.join(work, "walkthrough-tools.yaml")
    yaml.safe_dump({"toolset": {"tools": hw_tools}}, open(tools_values, "w"))
    toolset_extra += ["-f", tools_values]

walk = {
    "tenant": render("tenant-provision", os.path.join(ex, "tenants", "hello-world.yaml")),
    "inference": render("inference-provision", os.path.join(ex, "inference", "qwen3-5-2b.yaml")),
    "toolset": render("toolset-provision", os.path.join(ex, "toolsets", "hello-world", "values.yaml"), toolset_extra),
    "workspace": render("workspace-provision", os.path.join(ex, "workspaces", "hello-world.yaml")),
    "channel": render("channel-provision", os.path.join(ex, "channels", "tailnet.yaml")),
}

def where(kind):
    d = walk[kind][0]
    return d and (d["metadata"]["name"], d["metadata"]["namespace"])

for kind, (doc, err) in walk.items():
    check(f"walkthrough: {kind} example renders one command", doc is not None, err)

t = walk["tenant"][0]
check("walkthrough: tenant command provisions tenant hello-world",
      where("tenant") == ("provision-tenant-hello-world", "sycophant-system") and (t.get("data") or {}).get("tenant") == "hello-world",
      f"got: {where('tenant')} tenant={t and (t.get('data') or {}).get('tenant')}")
for kind, name in (("inference", "provision-inference-qwen3-5-2b"),
                   ("toolset", "provision-toolset-hello-world"),
                   ("workspace", "provision-workspace-hello-world"),
                   ("channel", "provision-channel-tailnet")):
    check(f"walkthrough: {kind} command is {name} in tenant-hello-world",
          where(kind) == (name, "tenant-hello-world"), f"got: {where(kind)}")

w = walk["workspace"][0]
wdata = (w and w.get("data")) or {}
got = {k: v for k, v in wdata.items() if k in ("toolsets", "models") or k.startswith("grant.")}
want = {"toolsets": "hello-world", "models": "qwen3-5-2b",
        "grant.hello-world.demo-key.secret": "demo-ssh-key",
        "grant.hello-world.demo-key.path": "/home/agent/.ssh/id_ed25519"}
check("walkthrough: workspace binds toolset hello-world, model qwen3-5-2b and grant hello-world/demo-key",
      got == want, f"got: {got}")

if not isinstance(hw_tools, dict):
    check("hello-world tools.yaml holds the stdlib tools and the demo tools", False,
          f"missing {os.path.relpath(hw_tools_path, root)}")
else:
    lost = sorted(k for k, v in base_tools.items() if hw_tools.get(k) != v)
    check("hello-world tools.yaml keeps every stdlib tools.yaml key and value", not lost and bool(base_tools),
          f"missing or changed: {lost}")
    extra = {k: v for k, v in hw_tools.items() if k not in base_tools}
    def args_of(tool):
        return sorted({m.group(1) for k in extra for m in [re.match(rf"^tool\.{tool}\.arg\.([^.]+)\.", k)] if m})
    stray = sorted(k for k in extra if not re.match(r"^tool\.(GetSecret|WebFetch|ShowKernel)\.", k))
    shape = {
        "GetSecret": args_of("GetSecret"), "WebFetch": args_of("WebFetch"), "ShowKernel": args_of("ShowKernel"),
        "descriptions": sorted(t for t in ("GetSecret", "WebFetch", "ShowKernel") if f"tool.{t}.description" in extra),
        "name": (extra.get("tool.GetSecret.arg.name.type"), extra.get("tool.GetSecret.arg.name.required")),
        "url": (extra.get("tool.WebFetch.arg.url.type"), extra.get("tool.WebFetch.arg.url.required")),
    }
    want_shape = {
        "GetSecret": ["name"], "WebFetch": ["url"], "ShowKernel": [],
        "descriptions": ["GetSecret", "ShowKernel", "WebFetch"],
        "name": ("string", "true"), "url": ("string", "true"),
    }
    check("hello-world tools.yaml adds GetSecret(name), WebFetch(url) and ShowKernel()",
          shape == want_shape and not stray, f"got: {shape}, stray keys: {stray}")

dockerfile = os.path.join(ex, "toolsets", "hello-world", "Dockerfile")
lines = [l.strip() for l in open(dockerfile)] if os.path.isfile(dockerfile) else None
froms = [l.split()[1] for l in lines or [] if re.match(r"(?i)^FROM\s", l)]
base_arg = [l for l in lines or [] if re.match(r"(?i)^ARG\s+BASE_IMAGE\b", l)]
check("hello-world Dockerfile builds FROM toolset:local and declares no BASE_IMAGE arg",
      lines is not None and froms == ["toolset:local"] and not base_arg,
      f"FROM {froms}, BASE_IMAGE args {base_arg}" if lines is not None else "missing Dockerfile")

doc = walk["toolset"][0]
d = os.path.join(work, "walkthrough-toolset")
os.makedirs(d)
if doc is not None:
    yaml.safe_dump(doc, open(os.path.join(d, "command.yaml"), "w"))
    m = doc["metadata"]
    yaml.safe_dump({"apiVersion": "cli.kyverno.io/v1alpha1", "kind": "Test",
                    "metadata": {"name": "command-vap-walkthrough-toolset"}, "policies": ["../policy.yaml"],
                    "resources": ["command.yaml"], "userinfo": "../operator.yaml",
                    "variables": "../values.yaml",
                    "results": [{"policy": "command-provision-toolset", "isValidatingAdmissionPolicy": True,
                                 "kind": "ConfigMap", "result": "pass",
                                 "resources": [f"{m['namespace']}/{m['name']}"]}]},
                   open(os.path.join(d, "kyverno-test.yaml"), "w"))

open(os.path.join(work, "checks"), "a").write("".join(checks))
PY


results() {
  [ -f "$WORK/$1.json" ] || kyverno test "$WORK/$1" -o json --detailed-results --remove-color 2>/dev/null \
    | sed -n '/^\[/,/^\]/p' | jq -s 'add // []' > "$WORK/$1.json"
  cat "$WORK/$1.json"
}

expect() {
  local dir="$1" resource="$2" token="$3" out row
  out="$(results "$dir")"
  row="$(jq -c --arg r "$resource" '[.[] | select(.RESOURCE | endswith("/" + $r))][0] // empty' <<<"$out")"
  if [ -z "$row" ]; then
    bad "$dir/$resource" "no result reported"
  elif [ "$(jq -r .RESULT <<<"$row")" != "Pass" ]; then
    bad "$dir/$resource" "$(jq -r '.REASON + ": " + (.Message // "")' <<<"$row")"
  elif [ -n "$token" ] && ! jq -e --arg t "$token" '(.Message // "") | contains($t)' <<<"$row" >/dev/null; then
    bad "$dir/$resource" "rejection does not name '$token': $(jq -r '.Message // ""' <<<"$row")"
  else
    ok "$dir/$resource"
  fi
}

expect tenant-unknown-key provision-tenant-acme bogus
expect tenant-from-serviceaccount provision-tenant-acme issuer-prefixed
expect tenant-token-over-cap "provision-tenant-$(printf 'a%.0s' {1..47})" "at most 46"
expect tenant-missing-capacity provision-tenant-acme capacity
expect tenant-missing-owner provision-tenant-acme owner
expect tenant-bad-owner-hash provision-tenant-acme owner-hash
expect tenant-name-mismatch provision-tenant-other "name must be provision-tenant-<tenant>"
expect tenant-owner-not-sender provision-tenant-acme "'owner' must equal its sender"
expect inference-commands provision-inference-format format
expect inference-commands provision-inference-bool server.singleNodeException
expect inference-commands provision-inference-localbaseurl "must not carry a 'baseUrl'"
expect inference-commands provision-inference-singlelabel "'baseUrl' must be an http or https URL whose host is a dotted name"
expect inference-commands provision-inference-emptyimage "non-empty 'server.image'"
expect toolset-commands provision-toolset-tagimage "'image' must"
expect toolset-commands provision-toolset-bogus "unknown key 'tool.git-status.bogus'"
expect toolset-commands provision-toolset-badtype "'tool.git-clone.arg.url.type' must"
expect toolset-commands provision-toolset-deadline "'deadlineSeconds' must"
expect toolset-commands provision-toolset-noenv "'tool.git-clone.arg.url.env'"
expect toolset-commands provision-toolset-dotted "unknown key 'tool.git.status.description'"
expect toolset-commands provision-toolset-required "'tool.git-clone.arg.url.required' must"
expect toolset-commands reap-toolset-data "'image'"
expect workspace-commands provision-workspace-unlisted "'grant.hello-world.demo-key.secret' grants a toolset not in 'toolsets'"
expect workspace-commands provision-workspace-nosecret "'grant.hello-world.demo-key.path' has no '.secret' beside it"
expect workspace-commands provision-workspace-undottedegress "'grant.hello-world.demo-key.egress' must be a dotted hostname"
expect workspace-commands provision-workspace-relativepath "'grant.hello-world.demo-key.path' must be an absolute path"
expect workspace-commands provision-workspace-badsecret "a workspace grant's 'secret' must be a Secret name"
expect instructions-commands provision-instructions-owned ""
expect suspend-bad-keepalive suspend-tenant "0, 30, 60, 90"
expect slice-commands-validate-data "provision-workspace-$(printf 'a%.0s' {1..50})" "at most 49"
expect slice-commands-validate-data "provision-toolset-$(printf 'a%.0s' {1..51})" "at most 50"
expect slice-commands-validate-data "provision-inference-$(printf 'a%.0s' {1..50})" "at most 49"
expect slice-commands-validate-data "provision-channel-$(printf 'a%.0s' {1..51})" "at most 50"
expect slice-commands-validate-data provision-channel-kind "unknown key 'kind'"
expect slice-commands-validate-data provision-channel-nologin "non-empty 'loginServer'"
expect slice-commands-validate-data provision-user-0f1e2d3c4 handle
expect slice-commands-validate-data provision-user-1f1e2d3c4b quote
expect slice-commands-validate-data provision-user-2f1e2d3c4b quote
expect slice-commands-validate-data reap-user-306b623ce6 ""
expect slice-commands-validate-data reap-user-306b623ce "reap-user-<handle>"
expect slice-commands-validate-data reap-user-306b623ce63 "reap-user-<handle>"
expect slice-commands-validate-data reap-user-306B623CE6 "reap-user-<handle>"
expect slice-commands-validate-data reap-user-306b623cgz "reap-user-<handle>"
expect walkthrough-toolset provision-toolset-hello-world ""
expect tenant-headscale-bad-bool provision-tenant-acme "headscale.enabled"
expect tenant-headscale-no-serverurl provision-tenant-acme "non-empty 'headscale.serverUrl'"
expect tenant-headscale-bad-serverurl provision-tenant-acme "'headscale.serverUrl' must be an http or https URL"
expect tenant-headscale-no-acme-email provision-tenant-acme "non-empty 'headscale.acme.email'"
expect tenant-headscale-bad-basedomain provision-tenant-acme "'headscale.baseDomain' must be a DNS name"
expect tenant-headscale-bad-storageclass provision-tenant-acme "'headscale.storageClass' must be a Kubernetes name"
expect tenant-headscale-bad-email provision-tenant-acme "'headscale.acme.email' must be a valid email address"
# The user command's data contract: subject + workspaces + handles.
expect user-commands provision-user-4d5e6f7a81 ""
expect user-commands provision-user-4d5e6f7a82 "comma list of DNS-safe names"
expect user-commands provision-user-4d5e6f7a83 "handle.<channel>"
expect user-commands provision-user-4d5e6f7a84 "control character"
# A standing ServiceAccount credential cannot write a user command.
expect user-from-serviceaccount provision-user-4d5e6f7a81 "issuer-prefixed"
while IFS=$'\t' read -r res name detail; do
  if [ "$res" = ok ]; then ok "$name"; else bad "$name" "$detail"; fi
done < "$WORK/checks"
while read -r name; do expect commands "$name" ""; done < "$WORK/admitted"

open="$(python3 - "$WORK/policy.yaml" <<'PY'
import sys, yaml
vaps = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d and d["kind"] == "ValidatingAdmissionPolicy"]
print(" ".join(d["metadata"]["name"] for d in vaps if d["spec"].get("failurePolicy") != "Fail") if vaps else "none rendered")
PY
)"
if [ -z "$open" ]; then ok "every command VAP fails closed"; else bad "every command VAP fails closed" "$open"; fi

summary
