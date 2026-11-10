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
  --show-only templates/command-vap.yaml >"$WORK/policy.yaml" || exit 1
OPERATOR="https://sycophant-dex.sycophant-system.svc.cluster.local:5556/dex#operator" # editorconfig-checker-disable-line
helm template c "$ROOT/charts/sycophant-cluster" -n kyverno \
  --api-versions policies.kyverno.io/v1/ValidatingPolicy \
  --set selfService.enabled=true \
  --set-string "selfService.operator=$OPERATOR" \
  --show-only templates/command-vap.yaml >"$WORK/policy-self-service.yaml" ||
  exit 1

python3 - "$WORK" "$HERE" "$ROOT" <<'PY'
import hashlib, os, re, subprocess, sys, yaml
work, here, root = sys.argv[1:4]
chart = os.path.join(root, "charts", "sycophant-commands")

def hex10(target):
    return hashlib.sha256(target.encode()).hexdigest()[:10]

case = os.path.join(work, "commands")
os.makedirs(case)
checks = []
def check(name, good, detail=""):
    checks.append(f"{'ok' if good else 'bad'}\t{name}\t{detail}\n")

def render(template, values, extra=()):
    if not os.path.isfile(values):
        return None, f"missing {os.path.relpath(values, root)}"
    p = subprocess.run(
        [
            "helm", "template", "c", chart, "-f", values, *extra,
            "--show-only", f"templates/{template}.yaml"
        ],
        capture_output=True, text=True
    )
    if p.returncode != 0:
        return None, " ".join(
            l for l in p.stderr.split("\n")
            if l and not l.startswith("Use --debug flag"))
    docs = [d for d in yaml.safe_load_all(p.stdout) if d]
    if len(docs) != 1 or docs[0].get("kind") != "ConfigMap":
        return None, f"{len(docs)} objects, not one ConfigMap"
    return docs[0], ""

tdir = os.path.join(chart, "templates")
templates = (sorted(f[:-len(".yaml")] for f in os.listdir(tdir)
                    if f.endswith(".yaml")) if os.path.isdir(tdir) else [])

renders = [(t, "command-values.yaml", t) for t in templates]
renders.append(("inference-provision", "command-values-remote.yaml",
                "inference-provision-remote"))
rendered = {}
for template, values, out in renders:
    doc, err = render(template, os.path.join(here, values))
    if doc is None:
        check(f"render {out}", False, err)
        continue
    rendered[out] = doc
    yaml.safe_dump(doc, open(os.path.join(case, out + ".yaml"), "w"))

def summary(out):
    d = rendered.get(out)
    return (d and (
        d["metadata"]["name"], d["metadata"]["namespace"],
        d.get("data") or {}
    ))

def target(d):
    return d and (
        (d["metadata"].get("annotations") or {}).get("sycophant.md/target"))

want = ("provision-agent-" + hex10("hello-world"), "tenant-acme", {
    "toolsets": "hello-world",
    "model": "deepseek",
    "storage": "5Gi",
    "secret.hello-world.demo-ssh-key": "/home/agent/.ssh/id_ed25519",
    "secret.hello-world.demo.dotted": "/home/agent/.ssh/id_rsa",
    "egress.hello-world": "git.example.com,github.com",
})
check("agent values flatten to command data",
      summary("agent-provision") == want,
      f"got: {summary('agent-provision')}")

want = ("provision-tenant-8f6fc2b2f4", "sycophant-system",
        {
            "tenant": "8f6fc2b2f4", "capacity": "2Gi",
            "owner": (
                # editorconfig-checker-disable-next-line
                "https://sycophant-dex.sycophant-system.svc.cluster.local:5556/dex#operator"
            ),
            "owner-hash": "027164b38d531173", "headscale.enabled": "true",
            "headscale.serverUrl": "http://localhost:8080",
            "headscale.magicDns": "true", "headscale.baseDomain": "ts.local",
            "headscale.acme.enabled": "false",
            "headscale.acme.email": "ops@example.com"
        })
check("tenant values flatten to command data",
      summary("tenant-provision") == want,
      f"got: {summary('tenant-provision')}")
check("tenant command carries the chosen name acme in sycophant.md/target",
      target(rendered.get("tenant-provision")) == "acme",
      f"got: {target(rendered.get('tenant-provision'))}")

want = ("provision-toolset-" + hex10("hello-world"), "tenant-acme", {
    "image": "ghcr.io/sycophant-md/hello-world@sha256:"
    + "0123456789abcdef" * 4,
    "keepalive": "true",
    "deadlineSeconds": "300",
    "env.GIT_TERMINAL_PROMPT": "0",
    "tool.git-status.description": (
        "Show the working tree status in the workspace."),
    "tool.git-clone.description": "Clone a git repository.",
    "tool.git-clone.arg.url.type": "string",
    "tool.git-clone.arg.url.required": "true",
    "tool.git-clone.arg.url.env": "URL",
    "tool.git-clone.arg.url.description": "Repository URL.",
})
check("toolset values flatten to command data",
      summary("toolset-provision") == want,
      f"got: {summary('toolset-provision')}")

want = ("provision-user-b212e3ad41", "tenant-acme",
        {"user": "https://dex.example.com/dex#alice"})
check("user command named by the subject's sha256 handle",
      summary("user-provision") == want,
      f"got: {summary('user-provision')}")

want = ("provision-channel-" + hex10("app"), "tenant-acme", {
    "storageClass": "local-path",
    "image": "tailscale/tailscale",
    "tag": "v1.80.0",
    "pullPolicy": "Always",
    "loginServer": "http://headscale:8080",
    "hostname": "relay",
    "authKeySecret": "relay-tsnet-authkey",
})
check("channel values flatten to command data",
      summary("channel-provision") == want,
      f"got: {summary('channel-provision')}")

archive = yaml.safe_load(
    open(os.path.join(here, "command-values.yaml")))["instructions"]["archive"]
want = ("provision-instructions-" + hex10("hello-world"), "tenant-acme",
        {"archive": archive})
check("instructions values render to a command carrying only the archive",
      summary("instructions-provision") == want,
      f"got: {summary('instructions-provision')}")
want = ("reap-instructions-" + hex10("hello-world"), "tenant-acme", {})
check(
    "an instructions reap renders reap-instructions-<10hex of the agent> "
    "with no data",
    summary("instructions-reap") == want,
    f"got: {summary('instructions-reap')}")

targets = {
    "agent-provision": "hello-world", "agent-reap": "hello-world",
    "instructions-provision": "hello-world", "instructions-reap": "hello-world",
    "toolset-provision": "hello-world", "toolset-reap": "hello-world",
    "inference-provision": "local", "inference-reap": "local",
    "inference-provision-remote": "deepseek",
    "channel-provision": "app", "channel-reap": "app",
    "user-provision": "https://dex.example.com/dex#alice",
    "user-reap": "https://dex.example.com/dex#alice",
}
for out, tgt in sorted(targets.items()):
    d = rendered.get(out)
    kind, verb = out.split("-")[:2]
    got = d and (d["metadata"]["name"], target(d))
    check(
        f"{out} renders {verb}-{kind}-<10hex of target> with "
        f"sycophant.md/target {tgt}",
        got == (f"{verb}-{kind}-{hex10(tgt)}", tgt), f"got: {got}")
d = rendered.get("tenant-suspend")
check("tenant-suspend renders suspend-tenant with no sycophant.md/target",
      bool(d) and d["metadata"]["name"] == "suspend-tenant"
      and target(d) is None,
      f"got: {d and (d['metadata']['name'], target(d))}")

results, names = [], []
for out, d in sorted(rendered.items()):
    kind, verb = out.split("-")[:2]
    m = d["metadata"]
    names.append(m["name"])
    results.append({"policy": f"command-{verb}-{kind}",
                    "isValidatingAdmissionPolicy": True,
                    "kind": "ConfigMap", "result": "pass",
                    "resources": [f"{m['namespace']}/{m['name']}"]})
open(os.path.join(work, "admitted"), "w").write("".join(n + "\n"
                                                        for n in names))
yaml.safe_dump(
    {
        "apiVersion": "cli.kyverno.io/v1alpha1", "kind": "Test",
        "metadata": {"name": "command-vap-commands"},
        "policies": ["../policy.yaml"],
        "resources": [o + ".yaml" for o in sorted(rendered)],
        "userinfo": "../operator.yaml",
        "variables": "../values.yaml", "results": results
    },
    open(os.path.join(case, "kyverno-test.yaml"), "w")
)

# The walkthrough: the commands chart rendered from the example values files,
# the way `sycoadm <kind> up -f` renders them. The toolset render carries a
# digest-pinned image and its tools.yaml as `toolset.tools`, as the image push
# and its md.sycophant.tools label supply them; the command VAP must admit it
# and the inference render.
ex = os.path.join(root, "examples", "sycoadm")

def load(path):
    return yaml.safe_load(open(path)) if os.path.isfile(path) else None

hw_tools_path = os.path.join(ex, "toolsets", "hello-world", "tools.yaml")
hw_tools = load(hw_tools_path)
base_tools = load(os.path.join(root, "images", "toolset", "tools.yaml")) or {}

toolset_extra = [
    "--set-string",
    "toolset.image=registry.local/toolset-hello-world@sha256:" + "0" * 64
]
if isinstance(hw_tools, dict):
    tools_values = os.path.join(work, "walkthrough-tools.yaml")
    yaml.safe_dump({"toolset": {"tools": hw_tools}}, open(tools_values, "w"))
    toolset_extra += ["-f", tools_values]

walk = {
    "tenant": render(
        "tenant-provision",
        os.path.join(ex, "tenants", "hello-world.yaml")
    ),
    "inference": render("inference-provision",
                        os.path.join(ex, "inference", "qwen3-5-2b.yaml")),
    "toolset": render(
        "toolset-provision",
        os.path.join(ex, "toolsets", "hello-world", "values.yaml"),
        toolset_extra
    ),
    "agent": render("agent-provision",
                    os.path.join(ex, "agents", "hello-world.yaml")),
    "channel": render("channel-provision",
                      os.path.join(ex, "channels", "tailnet.yaml")),
}

def where(kind):
    d = walk[kind][0]
    return d and (
        d["metadata"]["name"], d["metadata"]["namespace"], target(d)
    )

for kind, (doc, err) in walk.items():
    check(f"walkthrough: {kind} example renders one command",
          doc is not None, err)

t = walk["tenant"][0]
thash = (t.get("data") or {}).get("tenant") if t else None
check(
    "walkthrough: tenant command provisions tenant hello-world as "
    "provision-tenant-<10hex>, data.tenant the 10hex, "
    "sycophant.md/target hello-world",
    bool(re.fullmatch(r"[a-f0-9]{10}", thash or ""))
    and where("tenant") == (f"provision-tenant-{thash}", "sycophant-system",
                            "hello-world"),
    f"got: {where('tenant')} tenant={thash}")
for kind, tgt, name in (
    (
        "inference", "qwen3-5-2b",
        "provision-inference-f5b5492557"
    ),
    ("toolset", "hello-world", "provision-toolset-afa27b44d4"),
    ("agent", "hello-world", "provision-agent-afa27b44d4"),
    ("channel", "tailnet", "provision-channel-ec6f049478")
):
    check(
        f"walkthrough: {kind} command is {name} (target {tgt}) in "
        "tenant-hello-world",
        where(kind) == (name, "tenant-hello-world", tgt),
        f"got: {where(kind)}")

inf = walk["inference"][0]
infdata = (inf and inf.get("data")) or {}
check(
    "walkthrough: the inference example turns the model's thinking off "
    "(server.thinking \"false\")",
    infdata.get("server.thinking") == "false",
    f"got: {infdata.get('server.thinking')}")

ag = walk["agent"][0]
agdata = (ag and ag.get("data")) or {}
got = {
    k: v for k, v in agdata.items()
    if k in ("toolsets", "model") or k.startswith("secret.")
    or k.startswith("egress.")
}
want = {"toolsets": "hello-world", "model": "qwen3-5-2b",
        "secret.hello-world.demo-ssh-key": "/home/agent/.ssh/id_ed25519"}
check(
    "walkthrough: agent binds toolset hello-world, model qwen3-5-2b and Secret "
    "demo-ssh-key on toolset hello-world",
    got == want, f"got: {got}")

keeps = "hello-world tools.yaml keeps every stdlib tools.yaml key and value"
adds = ("hello-world tools.yaml adds GetSecret(name), WebFetch(url) and "
        "ShowKernel()")
if not isinstance(hw_tools, dict):
    missing = f"missing {os.path.relpath(hw_tools_path, root)}"
    check(keeps, False, missing)
    check(adds, False, missing)
else:
    lost = sorted(k for k, v in base_tools.items() if hw_tools.get(k) != v)
    check(keeps, not lost and bool(base_tools), f"missing or changed: {lost}")
    extra = {k: v for k, v in hw_tools.items() if k not in base_tools}
    def args_of(tool):
        return (sorted({
            m.group(1) for k in extra
            for m in [re.match(rf"^tool\.{tool}\.arg\.([^.]+)\.", k)] if m
        }))
    stray = sorted(
        k for k in extra
        if not re.match(r"^tool\.(GetSecret|WebFetch|ShowKernel)\.", k)
    )
    shape = {
        "GetSecret": args_of("GetSecret"), "WebFetch": args_of("WebFetch"),
        "ShowKernel": args_of("ShowKernel"),
        "descriptions": sorted(t for t in (
            "GetSecret", "WebFetch",
            "ShowKernel"
        ) if f"tool.{t}.description" in extra),
        "name": (
            extra.get("tool.GetSecret.arg.name.type"),
            extra.get("tool.GetSecret.arg.name.required")
        ),
        "url": (extra.get("tool.WebFetch.arg.url.type"),
                extra.get("tool.WebFetch.arg.url.required")),
    }
    want_shape = {
        "GetSecret": ["name"], "WebFetch": ["url"], "ShowKernel": [],
        "descriptions": ["GetSecret", "ShowKernel", "WebFetch"],
        "name": ("string", "true"), "url": ("string", "true"),
    }
    check(adds, shape == want_shape and not stray,
          f"got: {shape}, stray keys: {stray}")

dockerfile = os.path.join(ex, "toolsets", "hello-world", "Dockerfile")
lines = (
    [l.strip() for l in open(dockerfile)]
    if os.path.isfile(dockerfile) else None
)
froms = [l.split()[1] for l in lines or [] if re.match(r"(?i)^FROM\s", l)]
check(
    "hello-world Dockerfile builds FROM toolset:local",
    lines is not None and froms == ["toolset:local"],
    f"FROM {froms}" if lines is not None else "missing Dockerfile")

for kind in ("inference", "toolset"):
    doc = walk[kind][0]
    d = os.path.join(work, f"walkthrough-{kind}")
    os.makedirs(d)
    if doc is not None:
        yaml.safe_dump(doc, open(os.path.join(d, "command.yaml"), "w"))
        m = doc["metadata"]
        yaml.safe_dump(
            {
                "apiVersion": "cli.kyverno.io/v1alpha1", "kind": "Test",
                "metadata": {"name": f"command-vap-walkthrough-{kind}"},
                "policies": ["../policy.yaml"],
                "resources": ["command.yaml"], "userinfo": "../operator.yaml",
                "variables": "../values.yaml",
                "results": [{
                    "policy": f"command-provision-{kind}",
                    "isValidatingAdmissionPolicy": True,
                    "kind": "ConfigMap", "result": "pass",
                    "resources": [f"{m['namespace']}/{m['name']}"]
                }]
            },
            open(os.path.join(d, "kyverno-test.yaml"), "w")
        )

open(os.path.join(work, "checks"), "w").write("".join(checks))
PY

expect tenant-unknown-key provision-tenant-8f6fc2b2f4 "unknown key 'bogus'"
expect tenant-from-serviceaccount provision-tenant-8f6fc2b2f4 \
  "admitted only for an issuer-prefixed user identity"
expect tenant-missing-capacity provision-tenant-8f6fc2b2f4 \
  "a provision command must carry a valid 'capacity' quantity"
expect tenant-missing-owner provision-tenant-8f6fc2b2f4 \
  "carry an 'owner' that starts with the IdP issuer prefix"
expect tenant-bad-owner-hash provision-tenant-8f6fc2b2f4 \
  "carry an 'owner-hash' that is 16 lowercase hex characters"
expect tenant-name-mismatch provision-tenant-d9298a10d1 \
  "a provision command's name must be provision-tenant-<tenant>"
expect tenant-owner-not-sender provision-tenant-10cc12a007 \
  "a provision command's 'owner' must equal its sender"
expect tenant-targets provision-tenant-e07ba6258f ""
expect tenant-targets provision-tenant-d5f1e46399 ""
expect tenant-targets provision-tenant-7c9786dd1b ""
expect tenant-targets provision-tenant-81f1238129 "sycophant.md/target"
expect tenant-targets provision-tenant-c59d4148b8 "sycophant.md/target"
expect tenant-targets provision-tenant-0789268956 "sycophant.md/target"
expect tenant-targets provision-tenant-e5d140fd46 "sycophant.md/target"
expect tenant-targets provision-tenant-acme "'tenant' that is the tenant hash"
expect tenant-targets provision-tenant-8F6FC2B2F4 \
  "'tenant' that is the tenant hash"
expect command-targets provision-agent-345837a29e "sycophant.md/target"
expect command-targets provision-agent-afa27b44d4 "sycophant.md/target"
expect command-targets provision-agent-hello-world \
  "ten lowercase hex characters"
expect command-targets reap-agent-afa27b44d4 ""
expect command-targets reap-agent-hello-world "ten lowercase hex characters"
expect command-targets reap-agent-345837a29e "sycophant.md/target"
expect command-targets provision-instructions-345837a29e "sycophant.md/target"
expect command-targets provision-instructions-hello-world \
  "ten lowercase hex characters"
expect command-targets provision-inference-345837a29e "sycophant.md/target"
expect command-targets provision-inference-hello-world \
  "ten lowercase hex characters"
expect command-targets provision-toolset-345837a29e "sycophant.md/target"
expect command-targets provision-toolset-hello-world \
  "ten lowercase hex characters"
expect command-targets reap-toolset-afa27b44d4 "sycophant.md/target"
expect command-targets provision-channel-345837a29e "sycophant.md/target"
expect command-targets provision-channel-hello-world \
  "ten lowercase hex characters"
expect command-targets provision-user-97ed537eff "sycophant.md/target"
expect inference-commands provision-inference-e904c9ccfa \
  "an inference command's 'format' must be one of"
expect inference-commands provision-inference-b760f44fa5 \
  "an inference command's 'server.singleNodeException' must be"
expect inference-commands provision-inference-6ee1642966 \
  "an inference command's 'server.thinking' must be"
expect inference-commands provision-inference-d7ebef78b1 ""
expect inference-commands provision-inference-4fce9b252c \
  "must not carry a 'baseUrl'"
expect inference-commands provision-inference-91be4365b5 \
  "'baseUrl' must be an http or https URL whose host is a dotted name"
expect inference-commands provision-inference-42ca36b09d \
  "non-empty 'server.image'"
expect toolset-commands provision-toolset-4854640c56 "'image' must"
expect toolset-commands provision-toolset-f3c16f525a \
  "unknown key 'tool.git-status.bogus'"
expect toolset-commands provision-toolset-fee7260343 \
  "'tool.git-clone.arg.url.type' must"
expect toolset-commands provision-toolset-dfc8aeb398 "'deadlineSeconds' must"
expect toolset-commands provision-toolset-3bfeeba98f \
  "'tool.git-clone.arg.url.env'"
expect toolset-commands provision-toolset-efa8efec6e \
  "unknown key 'tool.git.status.description'"
expect toolset-commands provision-toolset-d0a3630555 \
  "'tool.git-clone.arg.url.required' must"
expect toolset-commands reap-toolset-3a6eb0790f "'image'"
# An agent command names one model and gives each toolset its Secrets and
# egress domains.
expect agent-commands provision-agent-f0c711d6d4 "toolset not in 'toolsets'"
expect agent-commands provision-agent-89e8d91f57 "toolset not in 'toolsets'"
expect agent-commands provision-agent-7c6767e458 \
  "'secret.hello-world.demo-key' must be an absolute path"
expect agent-commands provision-agent-53d1bfc6d0 \
  "'secret.hello-world.demo-key' must be an absolute path"
expect agent-commands provision-agent-9fd76d24c5 \
  "'secret.hello-world.demo-key' must not be a reserved mount path"
expect agent-commands provision-agent-9ab9e6c844 \
  "'secret.hello-world.demo-key' must not be a reserved mount path"
expect agent-commands provision-agent-b8489bb62f \
  "two Secrets of one toolset on one path"
allowed_keys="allowed keys are toolsets, model, storage, archives,"
allowed_keys+=" secret.<toolset>.<secret-name> or egress.<toolset>"
expect agent-commands provision-agent-3c327826a7 \
  "unknown key 'secret.hello-world.Demo_Key'; $allowed_keys"
expect agent-commands provision-agent-bcc1af1ee3 \
  "'egress.hello-world' must be a comma list of dotted hostnames"
expect agent-commands provision-agent-2c28388c85 \
  "'egress.hello-world' must be a comma list of dotted hostnames"
expect agent-commands provision-agent-91cacd82ca \
  "an agent command must name exactly one 'model'"
expect agent-commands provision-agent-4a66f6b0e3 \
  "an agent command must name exactly one 'model'"
expect agent-commands provision-agent-185021e7bb ""
expect agent-commands provision-agent-95dd8bac0e \
  "an agent command must name exactly one 'model'"
expect agent-commands provision-agent-b4a614c05e ""
archives_rule="an agent command's 'archives' must be"
archives_rule+=" a whole number from 1 to 1000"
expect agent-commands provision-agent-8de3fe7bf4 "$archives_rule"
expect agent-commands provision-agent-6c2ce3ff2a "$archives_rule"
expect agent-commands provision-agent-4faccc6619 "$archives_rule"
expect agent-commands provision-agent-eaecbcc1a1 "$archives_rule"
expect agent-commands provision-agent-fab5c32748 "$archives_rule"
expect agent-commands provision-agent-8031a53de8 "$archives_rule"
expect agent-commands provision-agent-9c76a6aa51 "$archives_rule"
expect agent-commands provision-agent-b9ed80d2e7 "$archives_rule"
expect agent-commands provision-agent-85560b3a97 ""
expect agent-commands provision-agent-6392fd4c30 ""
expect agent-commands provision-agent-06713697f9 ""
# The same rules hold for a self-service sender: an issuer-prefixed tenant
# owner on the selfService render.
expect agent-self-service provision-agent-83ff6c98de \
  "two Secrets of one toolset on one path"
# An agent name is capped at 49 characters.
expect agent-commands provision-agent-160b4e433e "at most 49"
expect agent-commands provision-agent-8f9bec6a62 ""
expect agent-commands reap-agent-160b4e433e "at most 49"
expect agent-commands reap-agent-8f9bec6a62 ""
expect instructions-commands provision-instructions-f5e6d024c0 ""
expect suspend-bad-keepalive suspend-tenant "0, 30, 60, 90"
expect slice-commands-validate-data provision-toolset-bfc5fe0e36 "at most 50"
expect slice-commands-validate-data provision-inference-160b4e433e "at most 49"
expect slice-commands-validate-data provision-channel-bfc5fe0e36 "at most 50"
expect slice-commands-validate-data provision-channel-ba9abeca6d \
  "non-empty 'loginServer'"
expect slice-commands-validate-data provision-user-0f1e2d3c4 \
  "provision-user-<10hex>"
expect slice-commands-validate-data provision-user-1f1e2d3c4b \
  "a user command's 'user' must be an issuer-prefixed subject"
expect slice-commands-validate-data provision-user-2f1e2d3c4b \
  "a user command's 'user' must be an issuer-prefixed subject"
expect slice-commands-validate-data reap-user-306b623ce6 ""
expect slice-commands-validate-data reap-user-306b623ce "reap-user-<10hex>"
expect slice-commands-validate-data reap-user-306b623ce63 "reap-user-<10hex>"
expect slice-commands-validate-data reap-user-306B623CE6 "reap-user-<10hex>"
expect slice-commands-validate-data reap-user-306b623cgz "reap-user-<10hex>"
expect walkthrough-toolset provision-toolset-afa27b44d4 ""
expect walkthrough-inference provision-inference-f5b5492557 ""
expect tenant-headscale-bad-bool provision-tenant-8f6fc2b2f4 \
  "a provision command's 'headscale.enabled' must be"
expect tenant-headscale-no-serverurl provision-tenant-8f6fc2b2f4 \
  "non-empty 'headscale.serverUrl'"
expect tenant-headscale-bad-serverurl provision-tenant-8f6fc2b2f4 \
  "'headscale.serverUrl' must be an http or https URL"
expect tenant-headscale-no-acme-email provision-tenant-8f6fc2b2f4 \
  "non-empty 'headscale.acme.email'"
expect tenant-headscale-bad-basedomain provision-tenant-8f6fc2b2f4 \
  "'headscale.baseDomain' must be a DNS name"
expect tenant-headscale-bad-storageclass provision-tenant-8f6fc2b2f4 \
  "'headscale.storageClass' must be a Kubernetes name"
expect tenant-headscale-bad-email provision-tenant-8f6fc2b2f4 \
  "'headscale.acme.email' must be a valid email address"
# The user command's data contract: subject + agents + handles.
expect user-commands provision-user-4d5e6f7a81 ""
expect user-commands provision-user-4d5e6f7a82 "comma list of DNS-safe names"
expect user-commands provision-user-4d5e6f7a83 "handle.<channel>"
expect user-commands provision-user-4d5e6f7a84 \
  "each handle value in a user command must be a non-empty platform user ID"
# A standing ServiceAccount credential cannot write a user command.
expect user-from-serviceaccount provision-user-4d5e6f7a81 \
  "admitted only for an issuer-prefixed user identity"
while IFS=$'\t' read -r res name detail; do
  if [ "$res" = ok ]; then ok "$name"; else bad "$name" "$detail"; fi
done <"$WORK/checks"
while read -r name; do expect commands "$name" ""; done <"$WORK/admitted"

open="$(
  python3 - "$WORK/policy.yaml" <<'PY'
import sys, yaml
vaps = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d
        and d["kind"] == "ValidatingAdmissionPolicy"]
print(" ".join(
    d["metadata"]["name"] for d in vaps
    if d["spec"].get("failurePolicy") != "Fail"
) if vaps
      else "none rendered")
PY
)"
if [ -z "$open" ]; then
  ok "every command VAP fails closed"
else
  bad "every command VAP fails closed" "$open"
fi

summary
