#!/usr/bin/env bash
# Offline: renders the install policies from the cluster chart, generates each
# kind's install objects from a command with `kyverno apply`, then runs the
# generated Job script against stub helm and kubectl. No cluster.
#
#   bash tests/unit/provision-installer/acceptance.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE
source "$HERE/../lib.sh"
CHART="$ROOT/charts/sycophant-cluster"
readonly CHART

render() {
  helm template c "$CHART" -n kyverno \
    --api-versions policies.kyverno.io/v1/ValidatingPolicy "$@"
}

generate() {
  mkdir -p "$WORK/$2-out"
  kyverno apply "$WORK/$1-policy.yaml" --resource "$HERE/$2.yaml" \
    -f "$HERE/values.yaml" -o "$WORK/$2-out" --remove-color >/dev/null 2>&1
}
for kind in tenant agent user toolset inference channel instructions; do
  render --show-only "templates/$kind-kpol.yaml" >"$WORK/$kind-policy.yaml"
  generate "$kind" "$kind"
done
generate agent reap-agent
generate instructions reap-instructions
render >"$WORK/cluster.yaml"

mkdir -p "$WORK/bin"
cat >"$WORK/bin/kubectl" <<'SH'
#!/bin/sh
printf 'kubectl %s\n' "$*" >> "$CALLS"
case "$*" in
  # editorconfig-checker-disable-next-line
  *"get configmap"*"-o json"*) [ -n "$GET_CM_EXIT" ] && exit "$GET_CM_EXIT"; printf '{"metadata":{"uid":"%s","creationTimestamp":"2026-01-01T00:00:00Z","annotations":{"sycophant.md/target":"%s"}},"data":%s}' "$ORIG_UID" "$STUB_TARGET" "$GET_CM" ;;
  *"get deployment"*)
    [ -n "$GET_DEPLOY_EXIT" ] && exit "$GET_DEPLOY_EXIT"
    printf 'deployment.apps/ag-one\n' ;;
esac
exit 0
SH
cat >"$WORK/bin/helm" <<'SH'
#!/bin/sh
printf 'helm %s\n' "$*" >> "$CALLS"
exit "$HELM_EXIT"
SH
cat >"$WORK/bin/curl" <<'SH'
#!/bin/sh
printf 'curl %s\n' "$*" >> "$CALLS"
# An upload (`-T <file> <url>`) lands in $HOME/store/<path under the agent's
# folder>, so a case can compare what the store holds with the archive.
body='' url=''
while [ $# -gt 0 ]; do
  case "$1" in
    -T) body="$2"; shift ;;
    http://*|https://*) url="$1" ;;
  esac
  shift
done
if [ -n "$body" ]; then
  dest="$HOME/store/${url#*:ag-one/}"
  mkdir -p "$(dirname "$dest")"
  cp "$body" "$dest"
fi
exit 0
SH
# yq-go as the provisioner image ships it, for the job's one call,
# `yq eval '.' -`: it exits non-zero when stdin is not valid YAML.
PY=$(python3 -c 'import sys; print(sys.executable)')
cat >"$WORK/bin/yq" <<SH
#!$PY
SH
cat >>"$WORK/bin/yq" <<'SH'
import os, sys, yaml
with open(os.environ["CALLS"], "a") as fh:
    fh.write("yq " + " ".join(sys.argv[1:]) + "\n")
try:
    yaml.safe_load(sys.stdin)
except yaml.YAMLError as e:
    sys.stderr.write(f"Error: {e}\n")
    sys.exit(1)
SH
# BusyBox tar as the provisioner image ships it (v1.37.0), for the job's two
# calls, `-tvzf FILE` and `-xzf FILE -C DIR`: both strip a leading '/' or
# '../' with a stderr notice and exit 0; links list with ' -> '.
cat >"$WORK/bin/tar" <<SH
#!$PY
SH
cat >>"$WORK/bin/tar" <<'SH'
import os, sys, tarfile, time
with open(os.environ["CALLS"], "a") as fh:
    fh.write("tar " + " ".join(sys.argv[1:]) + "\n")
args = sys.argv[1:]
mode, archive = args[0][1], args[1]
dest = args[3] if mode == "x" else "."
tf = tarfile.open(archive, mode="r:gz")
warned = set()
def strip(name):
    while True:
        for p in ("/", "../"):
            if name.startswith(p):
                if p not in warned:
                    sys.stderr.write(
                        f"tar: removing leading '{p}' from member names\n")
                    warned.add(p)
                name = name[len(p):]
                break
        else:
            return name
for m in tf:
    name = strip(m.name)
    if mode == "t":
        kind = "d" if m.isdir() else "l" if m.issym() else "-"
        perm = "".join(c if m.mode & (1 << (8 - n)) else "-"
                        for n, c in enumerate("rwxrwxrwx"))
        stamp = time.strftime("%Y-%m-%d %H:%M:%S", time.gmtime(m.mtime))
        link = f" -> {m.linkname}" if m.issym() else ""
        print(f"{kind}{perm} {m.uid}/{m.gid} {m.size:>9} {stamp} {name}{link}")
    else:
        path = os.path.join(dest, name)
        if m.isdir():
            os.makedirs(path, exist_ok=True)
        elif m.isfile():
            os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
            with open(path, "wb") as out:
                out.write(tf.extractfile(m).read())
SH
chmod +x "$WORK"/bin/*

python3 - "$WORK" "$HERE" <<'PY'
import base64, io, json, os, re, shutil, subprocess, sys, tarfile, yaml

work = sys.argv[1]
uid10 = "0a1b2c3d4e"
TENANT_NS = "tenant-8f6fc2b2f4"
cases = {
    "tenant": {
        "command": "provision-tenant-8f6fc2b2f4",
        "command_ns": "sycophant-system",
        "target": "acme", "ns": TENANT_NS
    },
    "agent": {"command": "provision-agent-7692c3ad35",
              "command_ns": "tenant-acme", "release": "ag-one",
              "target": "one", "ns": "tenant-acme"},
    "user": {
        "command": "provision-user-0123456789",
        "release": "user-0123456789",
        "target": (
            # editorconfig-checker-disable-next-line
            "https://sycophant-dex.sycophant-system.svc.cluster.local:5556/dex#alice"
        ),
    },
    "toolset": {"command": "provision-toolset-7692c3ad35", "release": "ts-one",
                "target": "one"},
    "inference": {"command": "provision-inference-25bf8e1a23",
                  "release": "inf-local", "target": "local"},
    "channel": {"command": "provision-channel-a172cedcae", "release": "ch-app",
                "target": "app"},
    "instructions": {
        "command": "provision-instructions-7692c3ad35",
        "target": "one"
    },
}
passed = failed = 0

def ok(name):
    global passed
    passed += 1
    print(f"ok   {name}")

def bad(name, why):
    global failed
    failed += 1
    print(f"FAIL {name}\n    {why}")

def check(name, why):
    if why:
        bad(name, why)
    else:
        ok(name)

def generated(kind):
    out = os.path.join(work, f"{kind}-out")
    docs = []
    for f in sorted(os.listdir(out)) if os.path.isdir(out) else []:
        with open(os.path.join(out, f)) as fh:
            docs += [d for d in yaml.safe_load_all(fh) if d]
    return docs

def find(docs, kind, name, ns):
    for d in docs:
        m = d.get("metadata", {})
        if (d.get("kind") == kind and m.get("name") == name
            and m.get("namespace") == ns):
            return d
    return None

def show(calls):
    return [" ".join(a) for a in calls]

def listing(docs):
    return [(
        d["kind"], d["metadata"].get("name"),
        d["metadata"].get("namespace")
    ) for d in docs]

def job_env(job):
    c = job["spec"]["template"]["spec"]["containers"][0]
    return {e["name"]: e.get("value", "") for e in c.get("env", [])}

def run_job(job, helm_exit, get_cm="{}", get_cm_exit="", wrong_uid=False,
            get_deploy_exit="", target=None):
    c = job["spec"]["template"]["spec"]["containers"][0]
    script = c["args"][0].replace("/tmp/", work + "/scratch/")
    os.makedirs(work + "/scratch", exist_ok=True)
    calls = os.path.join(work, "calls")
    open(calls, "w").close()
    env = job_env(job)
    jq_dir = os.path.dirname(shutil.which("jq") or "/usr/local/bin/jq")
    if target is None:
        target = {c["command"]: c["target"]
                  for c in cases.values()}.get(env.get("COMMAND"), "one")
    env.update(
        PATH=f"{work}/bin:{jq_dir}:/usr/bin:/bin", CALLS=calls,
        HELM_EXIT=str(helm_exit), HOME=work, GET_CM=get_cm,
        GET_CM_EXIT=get_cm_exit, GET_DEPLOY_EXIT=get_deploy_exit,
        STUB_TARGET=target
    )
    # The stub always returns the command's real uid in its JSON.
    # ORIG_UID carries it; COMMAND_UID is what the Job
    # thinks its command's uid is.
    env["ORIG_UID"] = env.get("COMMAND_UID", "")
    if wrong_uid:
        env["COMMAND_UID"] = "wrong-uid-from-a-different-grant"
    rc = subprocess.run(["sh", "-c", script], env=env,
                        capture_output=True).returncode
    with open(calls) as fh:
        return rc, [l.split() for l in fh.read().splitlines()]

def selects(selector, labels):
    if (any(labels.get(k) != v
            for k, v in (selector.get("matchLabels") or {}).items())):
        return False
    return all(
        e["operator"] == "In" and labels.get(e["key"]) in e["values"]
        for e in selector.get("matchExpressions") or []
    )

def policy(kind, k8s_kind, name):
    return next((
        d for d in yaml.safe_load_all(open(os.path.join(
            work,
            f"{kind}-policy.yaml"
        )))
        if d and d.get("kind") == k8s_kind and d["metadata"]["name"] == name
    ), None)

def reaps(pol, docs):
    selector = pol["spec"]["matchConstraints"]["objectSelector"]
    expr = next(v["expression"] for v in pol["spec"]["variables"]
                if v["name"] == "reapAfter")
    annotation = re.search(r'annotations\["([^"]+)"\]', expr).group(1)
    return [f"{d['kind']}/{d['metadata']['name']}" for d in docs
            if not selects(selector, d["metadata"].get("labels") or {})
            or not (d["metadata"].get("annotations") or {}).get(annotation)]

def in_ns(argv, ns):
    return f"--namespace={ns}" in argv or any(
        argv[i] in ("-n", "--namespace") and argv[i + 1] == ns
        for i in range(len(argv) - 1))

def helms(calls):
    return [i for i, a in enumerate(calls) if a[0] == "helm"]

def upgrades(calls):
    return [i for i, a in enumerate(calls)
            if a[0] == "helm" and "upgrade" in a]

def command_deletes(calls, command, ns):
    return [i for i, a in enumerate(calls)
            if a[0] == "kubectl" and "delete" in a and command in a
            and any(x in ("configmap", "configmaps", "cm") for x in a)
            and in_ns(a, ns)]

# The per-kind install paths share one script body (the stale-command guard is
# the shared else branch), so tenant and agent stand for every kind here.
for kind in ("tenant", "agent"):
    c = cases[kind]
    docs = generated(kind)
    obj = f"provision-{kind}-{uid10}"
    sa = find(docs, "ServiceAccount", obj, "sycophant-system")
    job = find(docs, "Job", obj, "sycophant-system")
    rb = find(docs, "RoleBinding", obj, c["ns"])
    name = f"{kind}: install identity named from the command uid"
    if not (sa and job and rb
            and job["spec"]["template"]["spec"]
            .get("serviceAccountName") == obj
            and [s.get("name") for s in rb.get("subjects", [])] == [obj]):
        bad(name,
            f"want ServiceAccount and Job {obj} in sycophant-system and "
            f"RoleBinding {obj} in {c['ns']} using it; generated "
            f"{listing(docs)}")
        continue
    ok(name)

    check(f"{kind}: install RoleBinding binds ClusterRole provision-write",
          None if rb["roleRef"] == {"apiGroup": "rbac.authorization.k8s.io",
                                    "kind": "ClusterRole",
                                    "name": "provision-write"}
          else f"roleRef {rb['roleRef']}")

    if kind != "tenant":
        reap = policy(kind, "DeletingPolicy", f"reap-{kind}")
        miss = reaps(reap, (sa, rb)) if reap else ["no DeletingPolicy"]
        check(f"reap-{kind} selects this install SA and RoleBinding",
              None if not miss else f"not reaped: {miss}")

    rc, calls = run_job(job, 0)
    helm_at = upgrades(calls)
    del_at = command_deletes(calls, c["command"], c["command_ns"])
    check(f"{kind}: successful install deletes its command after helm",
          None if rc == 0 and helm_at and del_at and min(del_at) > max(helm_at)
          else f"rc={rc}, calls={show(calls)}")

    rc, calls = run_job(job, 1)
    check(f"{kind}: failed install leaves its command",
          None if rc != 0
          and not command_deletes(calls, c["command"], c["command_ns"])
          else f"rc={rc}, calls={show(calls)}")

    if kind != "tenant":
        # A retry that finds its command gone must not reach helm: empty
        # values would prune the release.
        rc, calls = run_job(job, 0, get_cm_exit="1")
        check(f"{kind}: a missing command stops the install before helm",
              None if rc != 0 and not helms(calls)
              else f"rc={rc}, calls={show(calls)}")

        rc, calls = run_job(job, 0, wrong_uid=True)
        check(f"{kind}: a replaced command stops the install before helm",
              None if rc != 0 and not helms(calls)
              else f"rc={rc}, calls={show(calls)}")

        # The tenant installer never reads its command (its Role is delete
        # only); admission refuses a tenant command with no target.
        name = (
            f"{kind}: an empty sycophant.md/target stops the install before "
            "helm and leaves its command")
        rc, calls = run_job(job, 0, target="")
        check(name, None if rc != 0 and not helms(calls)
              and not command_deletes(calls, c["command"], c["command_ns"])
              else f"rc={rc}, calls={show(calls)}")

def marker_calls(calls, verb):
    return [i for i, a in enumerate(calls) if a[0] == "kubectl" and verb in a
            and "provision-tenant-owner" in a
            and any(x in ("configmap", "configmaps", "cm") for x in a)
            and in_ns(a, TENANT_NS)]
docs = generated("tenant")
obj = f"provision-tenant-{uid10}"
t_job = find(docs, "Job", obj, "sycophant-system")
name = (
    "tenant: after helm, the installer recreates provision-tenant-owner in the "
    "tenant namespace, then deletes its command")
if not t_job:
    bad(name, "no tenant install Job")
else:
    rc, calls = run_job(t_job, 0)
    helm_at = upgrades(calls)
    dels = [i for i in marker_calls(calls, "delete")
            if "--ignore-not-found" in calls[i]
            and any(x in ("--wait", "--wait=true") for x in calls[i])]
    creates = marker_calls(calls, "create")
    cmd_del = command_deletes(calls, cases["tenant"]["command"],
                              "sycophant-system")
    check(name, None if rc == 0 and helm_at and dels and creates and cmd_del
          and max(helm_at) < min(dels) < min(creates) < min(cmd_del)
          else f"rc={rc}, calls={show(calls)}")
    rc, calls = run_job(t_job, 1)
    check("tenant: a failed install creates no provision-tenant-owner marker",
          None if rc != 0 and not marker_calls(calls, "create")
          else f"rc={rc}, calls={show(calls)}")

# kyverno apply does not emit the base policy's multi-document template, so
# this reads the rendered policy.
name = (
    "tenant: the generated Namespace is tenant-<data.tenant> and carries the "
    "chosen name in sycophant.md/target")
base = policy("tenant", "GeneratingPolicy", "provision-tenant-base")
var = {
    v["name"]: v["expression"]
    for v in (base or {}).get("spec", {}).get("variables", [])
}
tmpl = (next((g["template"]["value"]
              for g in (base or {}).get("spec", {}).get("generate", [])), ""))
ns_doc = tmpl.split("\n---")[0].replace("((", "'((").replace("))", "))'")
ns_tmpl = yaml.safe_load(ns_doc) if "kind: Namespace" in ns_doc else None
ann = (ns_tmpl or {}).get("metadata", {}).get("annotations") or {}
check(name, None if var.get("tenant") == '"tenant-" + object.data["tenant"]'
      and (ns_tmpl or {}).get(
          "metadata", {}).get("name") == "(( variables.tenant ))"
      and ann.get("sycophant.md/target") == "(( variables.target ))"
      and var.get("target") == (
          'object.metadata.?annotations["sycophant.md/target"]'
          '.orValue("")')
      else f"tenant var={var.get('tenant')!r}, "
      f"target var={var.get('target')!r}, Namespace template={ns_tmpl!r}")

rb = find(docs, "RoleBinding", obj, "sycophant-system")
check("tenant: install ServiceAccount bound to its own command-delete Role",
      None if rb and rb["roleRef"] == {
          "apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": obj
      }
      and [
          (s.get("kind"), s.get("name"), s.get("namespace"))
          for s in rb.get("subjects", [])
      ] == [("ServiceAccount", obj, "sycophant-system")]
      else f"got {rb}")

role = find(docs, "Role", obj, "sycophant-system")
rules = [(r.get("apiGroups"), r.get("resources"), r.get("verbs"),
          r.get("resourceNames")) for r in (role or {}).get("rules") or []]
check("tenant: the install's Role may delete only its own tenant command",
      None if rules == [
          ([""], ["configmaps"], ["delete"], [cases["tenant"]["command"]])
      ]
      else f"got {role}")

reap_t = policy("tenant", "DeletingPolicy", "reap-tenant")
t_sa = find(docs, "ServiceAccount", obj, "sycophant-system")
name = (
    "tenant: the scheduled sweep selects the install ServiceAccount, Role "
    "and RoleBinding")
if not (reap_t and t_sa):
    bad(name, "missing reap-tenant DeletingPolicy or install ServiceAccount")
else:
    cond = next(c["expression"] for c in reap_t["spec"]["conditions"]
                if c["name"] == "selector-matches-by-kind")
    kinds = {
        r for rule in reap_t["spec"]["matchConstraints"]["resourceRules"]
        for r in rule["resources"]
    }
    component = t_sa["metadata"]["labels"]["app.kubernetes.io/component"]
    check(name, None if f'"{component}"' in cond
          and {"namespaces", "serviceaccounts", "rolebindings", "roles"}
          <= kinds
          else f"component label not in selector or kinds {kinds}")

def install_call(kind, get_cm="{}"):
    job = find(
        generated(kind), "Job", f"provision-{kind}-{uid10}",
        "sycophant-system"
    )
    if not job:
        return None
    _, calls = run_job(job, 0, get_cm)
    return next((calls[i] for i in upgrades(calls)), None)

argv = install_call("tenant") or []
env = job_env(t_job) if t_job else {}
hs_file = [argv[i + 1] for i in range(len(argv) - 1) if argv[i] == "-f"]
# The headscale.* values in tenant.yaml.
hs_env = {
    "HS_ENABLED": "true", "HS_SERVER_URL": "https://hs.example.com",
    "HS_MAGIC_DNS": "false", "HS_BASE_DOMAIN": "ts.example.com",
    "HS_STORAGE_CLASS": "fast", "HS_ACME_ENABLED": "true",
    "HS_ACME_EMAIL": "ops@example.com"
}
check("tenant: install passes headscale values via -f /tmp/hs-values.yaml",
      None if t_job and argv and hs_file == [work + "/scratch/hs-values.yaml"]
      and {k: env.get(k) for k in hs_env} == hs_env
      and yaml.safe_load(open(hs_file[0])) == {"headscale": {
          "enabled": True, "serverUrl": "https://hs.example.com",
          "magicDns": False, "baseDomain": "ts.example.com",
          "storageClass": "fast",
          "acme": {"enabled": True, "email": "ops@example.com"}}}
      else f"argv={' '.join(argv)}, "
      f"env={ {k: env.get(k) for k in hs_env} }, hs_file={hs_file}")

check("tenant: the install targets the namespace tenant-<data.tenant>, "
      "the tenant hash",
      None if env.get("TENANT") == TENANT_NS
      else f"TENANT env is {env.get('TENANT')!r}, want {TENANT_NS}")

installs = {k: install_call(k)
            for k in ("agent", "user", "toolset", "inference", "channel")}
none = [k for k, a in installs.items() if a is None]
taking = [k for k, a in installs.items() if a and "--take-ownership" in a]
check("capability installs never take ownership of another release's objects",
      f"no helm upgrade call for {none}" if none
      else f"--take-ownership on {taking}" if taking else None)

argv = installs["user"] or []
check("user: install hands the command's write time to the chart",
      None if any(a.startswith("commandCreated=") for a in argv)
      else f"no commandCreated in {' '.join(argv)}")

problems = {}
for kind, short in (("tenant", None), ("inference", "local"),
                    ("toolset", "one"), ("agent", "one"), ("channel", "app"),
                    ("user", "0123456789")):
    command_data = (yaml.safe_load(open(os.path.join(
        sys.argv[2],
        f"{kind}.yaml"
    )))["data"] if short else {})
    argv = install_call(kind, json.dumps(command_data)) or []
    at = argv.index("--install") + 2 if "--install" in argv else len(argv)
    chart = argv[at] if at < len(argv) else None
    sets = [argv[i + 1] for i in range(len(argv) - 1)
            if argv[i] in ("--set", "--set-string")]
    files = [
        argv[i + 1] for i in range(len(argv) - 1)
        if argv[i] in ("-f", "--values")
    ]
    loaded = [yaml.safe_load(open(f)) for f in files if os.path.exists(f)]
    wrong = [w for w, bad_ in (
        ("own chart", chart != f"/charts/sycophant-{kind}"),
        (
            "tenant namespace.create=false", kind == "tenant"
            and "namespace.create=false" not in sets
        ),
        (
            "release from the sycophant.md/target "
            "(user: the command name's 10hex)",
            short and cases[kind]["release"] not in argv
        ),
        ("command data", short and not any(
            (v or {}).get("command")
            == command_data for v in loaded
        )),
        ("name=", short and f"name={short}" not in sets),
    ) if bad_]
    if wrong:
        problems[kind] = f"{wrong}: argv={' '.join(argv)}, values={loaded}"
check(
    "every kind's install runs its own chart under the command's release name "
    "and hands it the command data",
    problems)

# Every reap kind shares the uninstall script body; agent stands for all.
kind = "agent"
docs = generated(f"reap-{kind}")
command = cases[kind]["command"].replace("provision-", "reap-")
obj = f"uninstall-{kind}-{uid10}"
sa = find(docs, "ServiceAccount", obj, "sycophant-system")
job = find(docs, "Job", obj, "sycophant-system")
rb = find(docs, "RoleBinding", obj, "tenant-acme")
check(f"{kind}: reap command generates a uid-named uninstall identity "
      "bound to provision-write",
      None if sa and job and rb
      and job["spec"]["template"]["spec"].get("serviceAccountName") == obj
      and rb["roleRef"] == {
          "apiGroup": "rbac.authorization.k8s.io",
          "kind": "ClusterRole", "name": "provision-write"
      }
      and [
          (s.get("kind"), s.get("name"), s.get("namespace"))
          for s in rb.get("subjects", [])
      ] == [("ServiceAccount", obj, "sycophant-system")]
      else f"generated {listing(docs)}")

removes = (
    f"{kind}: uninstall removes the release, then deletes its reap command")
leaves = f"{kind}: failed uninstall leaves its reap command"
if job:
    rc, calls = run_job(job, 0)
    un_at = [
        i for i, a in enumerate(calls)
        if a[:2] == ["helm", "uninstall"] and cases[kind]["release"] in a
        and in_ns(a, "tenant-acme")
    ]
    del_at = command_deletes(calls, command, "tenant-acme")
    check(removes, None if rc == 0 and un_at and del_at
          and min(del_at) > max(un_at) and not upgrades(calls)
          else f"rc={rc}, calls={show(calls)}")
    rc, calls = run_job(job, 1)
    check(leaves, None if rc != 0
          and not command_deletes(calls, command, "tenant-acme")
          else f"rc={rc}, calls={show(calls)}")
else:
    bad(removes, "no uninstall Job")
    bad(leaves, "no uninstall Job")

reap = policy(kind, "DeletingPolicy", f"reap-{kind}")
name = f"{kind}: the scheduled sweep selects the uninstall identity"
if not (reap and sa and rb):
    bad(name, f"missing reap-{kind} DeletingPolicy or uninstall identity")
else:
    miss = [f"{d['kind']}/{d['metadata']['name']}" for d in (sa, rb)
            if (d["metadata"].get("labels") or {})
            .get("app.kubernetes.io/component") != f"uninstall-{kind}"]
    miss += reaps(reap, (sa, rb))
    check(name, None if not miss
          else f"{miss} not labelled uninstall-{kind} or not reaped")

def archive_of(members):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz",
                      format=tarfile.USTAR_FORMAT) as tf:
        for name, kind, payload in members:
            ti = tarfile.TarInfo(name)
            if kind == "dir":
                ti.type, ti.mode = tarfile.DIRTYPE, 0o755
                tf.addfile(ti)
            elif kind == "symlink":
                ti.type, ti.linkname = tarfile.SYMTYPE, payload
                tf.addfile(ti)
            else:
                ti.size, ti.mode = len(payload), 0o644
                tf.addfile(ti, io.BytesIO(payload))
    return base64.b64encode(buf.getvalue()).decode()

FOLDER = (
    "/buckets/sycophant-instructions/system:serviceaccount:tenant-acme:ag-one/")

def store_calls(calls):
    out = []
    for i, a in enumerate(calls):
        if a[0] != "curl":
            continue
        url = (next((
            x for x in a
            if "seaweedfs.sycophant-system.svc:8888" in x
        ), ""))
        method = (next((a[j + 1] for j in range(len(a) - 1)
                        if a[j] in ("-X", "--request")), ""))
        out.append((i, method.upper(), url))
    return out

def restarts(calls):
    return [i for i, a in enumerate(calls) if a[0] == "kubectl"
            and "rollout" in a and "restart" in a
            and "deployment/ag-one" in a
            and in_ns(a, "tenant-acme")]

docs = generated("instructions")
obj = f"provision-instructions-{uid10}"
inst = find(docs, "Job", obj, "sycophant-system")
inst_rb = find(docs, "RoleBinding", obj, "tenant-acme")
good = (archive_of([
    ("AGENTS.md", "file", b"You are the probe.\n"),
    ("docs", "dir", None), ("docs/deep.md", "file", b"deep\n")
]))
name = (
    "instructions: an install replaces the folder from the archive, restarts "
    "the agent, then deletes its command")
if not inst:
    bad(name,
        f"no Job {obj}; generated {listing(docs)}")
else:
    rc, calls = run_job(inst, 0, json.dumps({"archive": good}))
    store = store_calls(calls)
    listed = [i for i, a in enumerate(calls) if a[0] == "tar" and "t" in a[1]]
    wipe = [i for i, m, u in store if m == "DELETE"
            and u.endswith(FOLDER + "?recursive=true")]
    puts = {u.split(FOLDER, 1)[-1]: i for i, m, u in store if m == "PUT"
            and FOLDER in u}
    rs = restarts(calls)
    dels = command_deletes(
        calls, cases["instructions"]["command"], "tenant-acme")
    check(name, None if rc == 0 and listed and store
          and min(listed) < min(i for i, _, _ in store) and wipe
          and set(puts) == {"AGENTS.md", "docs/deep.md"}
          and min(puts.values()) > max(wipe)
          and rs and min(rs) > max(puts.values())
          and dels and min(dels) > max(rs) and not helms(calls)
          else f"rc={rc}, calls={show(calls)}")

    name = (
        "instructions: a failed agent lookup fails the job before the restart "
        "and leaves its command")
    rc, calls = run_job(inst, 0, json.dumps({"archive": good}),
                        get_deploy_exit="1")
    check(name, None if rc != 0 and not restarts(calls)
          and not command_deletes(calls, cases["instructions"]["command"],
                                  "tenant-acme")
          else f"rc={rc}, calls={show(calls)}")

    for label, members in (
        ("a symlink", [
            ("AGENTS.md", "file", b"x\n"),
            ("ln", "symlink", "/etc/passwd")
        ]),
        ("a path outside its folder", [
            ("AGENTS.md", "file", b"x\n"),
            ("../evil.md", "file", b"x\n")
        ]),
        ("an unsafe name", [("AGENTS.md", "file", b"x\n"),
                            ("a?b.md", "file", b"x\n")]),
    ):
        name = (
            f"instructions: an archive holding {label} fails before any store "
            "call and leaves its command")
        rc, calls = run_job(inst, 0,
                            json.dumps({"archive": archive_of(members)}))
        check(name, None if rc != 0 and not store_calls(calls)
              and not restarts(calls)
              and not command_deletes(calls, cases["instructions"]["command"],
                                      "tenant-acme")
              else f"rc={rc}, calls={show(calls)}")

    # Frontmatter is checked before any store call. A file
    # whose frontmatter is valid YAML is stored byte for byte (the uploader
    # never strips it); one whose frontmatter never closes or is not YAML
    # refuses the whole archive, so the current instructions stay.
    def stored(path):
        p = os.path.join(work, "store", path)
        return open(p, "rb").read() if os.path.isfile(p) else None

    big = b"---\n" + b"".join(
        b"key%04d: %s\n" % (i, b"v" * 60)
        for i in range(160)
    ) + b"---\nYou are the probe.\n"
    nested = b"---\ntitle: x\n---\nbody\n"
    name = (
        "instructions: files whose frontmatter is valid YAML (10 KiB, nested) "
        "are stored byte-equal to the archive")
    shutil.rmtree(os.path.join(work, "store"), ignore_errors=True)
    rc, calls = run_job(inst, 0, json.dumps({"archive": archive_of(
        [
            ("AGENTS.md", "file", big), ("sub", "dir", None),
            ("sub/x.md", "file", nested)
        ])}))
    got = {"AGENTS.md": stored("AGENTS.md"), "sub/x.md": stored("sub/x.md")}
    want = {"AGENTS.md": big, "sub/x.md": nested}
    check(name, None if len(big) >= 10240 and rc == 0 and got == want
          else f"rc={rc}, stored "
          f"equal={ {k: v == want[k] for k, v in got.items()} }, "
          f"calls={show(calls)}")

    for label, body in (
        (
            "frontmatter that is not valid YAML",
            b"---\nkey: [unclosed\n---\nbody"
        ),
        (
            "frontmatter that never closes",
            b"---\ntitle: x\nbody with no closer"
        ),
        (
            "a BOM before frontmatter that is not valid YAML",
            b"\xef\xbb\xbf---\nkey: [unclosed\n---\nbody"
        ),
        (
            "CRLF frontmatter that is not valid YAML",
            b"---\r\nkey: [unclosed\r\n---\r\nbody"
        ),
    ):
        name = (
            f"instructions: an AGENTS.md with {label} fails before any store "
            "call and leaves its command")
        shutil.rmtree(os.path.join(work, "store"), ignore_errors=True)
        rc, calls = run_job(inst, 0,
                            json.dumps({"archive": archive_of([(
                                "AGENTS.md",
                                "file", body
                            )])}))
        check(name, None if rc != 0 and not store_calls(calls)
              and not restarts(calls)
              and not command_deletes(calls, cases["instructions"]["command"],
                                      "tenant-acme")
              else f"rc={rc}, calls={show(calls)}")

cluster_text = open(os.path.join(work, "cluster.yaml")).read()
cluster = [d for d in yaml.safe_load_all(cluster_text) if d]

role_name = (inst_rb or {}).get("roleRef", {}).get("name")
role = find(cluster, "ClusterRole", role_name, None)
grants = sorted(
    (g, r, v) for rule in (role or {}).get("rules") or []
    for g in rule.get("apiGroups", [])
    for r in rule.get("resources", []) for v in rule.get("verbs", [])
)
want = sorted([
    ("apps", "deployments", "get"), ("apps", "deployments", "patch"),
    ("", "configmaps", "get"), ("", "configmaps", "delete")
])
check("instructions: the install binds a narrow ClusterRole, not "
      "provision-write",
      None if role_name and role_name != "provision-write" and grants == want
      else f"roleRef={role_name}, rules={grants}")

docs = generated("reap-instructions")
obj = f"uninstall-instructions-{uid10}"
reap_inst = find(docs, "Job", obj, "sycophant-system")
name = (
    "instructions: a reap removes the folder, restarts the agent, then deletes "
    "its reap command")
if not reap_inst:
    bad(name,
        f"no Job {obj}; generated {listing(docs)}")
else:
    rc, calls = run_job(reap_inst, 0)
    wipe = [i for i, m, u in store_calls(calls) if m == "DELETE"
            and u.endswith(FOLDER + "?recursive=true")]
    rs = restarts(calls)
    dels = command_deletes(
        calls,
        cases["instructions"]["command"].replace("provision-", "reap-"),
        "tenant-acme")
    check(name, None if rc == 0 and wipe and rs and min(rs) > max(wipe)
          and dels and min(dels) > max(rs)
          else f"rc={rc}, calls={show(calls)}")

blocks = re.split(r"\n---\n", cluster_text)

def identity_minimal():
    roles = [
        d for d in cluster if d.get("kind") == "ClusterRole"
        and d["metadata"]["name"] == "provision-identity"
    ]
    if len(roles) != 1:
        return (
            "provision-identity ClusterRole must render exactly once, got "
            f"{len(roles)}")
    role = roles[0]
    if role.get("rules") != []:
        return f"provision-identity must grant nothing, got {role.get('rules')}"
    if "aggregationRule" in role:
        return (
            "provision-identity must not aggregate rules from other "
            "ClusterRoles")
    agg = [
        k for k in role["metadata"].get("labels") or {}
        if "aggregate-to-" in k
    ]
    return (agg
            and "provision-identity must not aggregate into other "
            f"ClusterRoles: {agg}")
check("the provision-identity ClusterRole has no rules and no aggregation",
      identity_minimal())

# provision-write is static (no aggregationRule), so what it can do in a
# tenant namespace is exactly the rendered rules.
def write_scope():
    role = find(cluster, "ClusterRole", "provision-write", None)
    if role is None:
        return "no provision-write ClusterRole rendered"
    if "aggregationRule" in role:
        return (
            "provision-write must not aggregate rules from other ClusterRoles")
    reads = {"get", "list", "watch"}
    rules = [
        r for r in role.get("rules") or []
        if "secrets" in r.get("resources", [])
    ]
    unscoped = [r for r in rules if not r.get("resourceNames")]
    if not unscoped:
        return f"no unscoped secrets rule on provision-write: {rules}"
    uverbs = set().union(*[set(r.get("verbs", [])) for r in unscoped])
    if "create" not in uverbs:
        return f"provision-write cannot place a Secret: {sorted(uverbs)}"
    if uverbs & reads:
        return f"provision-write can read any Secret: {sorted(uverbs & reads)}"
    for r in rules:
        if r.get("resourceNames") and set(r.get("verbs", [])) & reads:
            if (sorted(r["resourceNames"]) != ["tailnet-identity"]
                or set(r["verbs"]) - {"get"}):
                return (
                    "scoped Secret read must be get on "
                    f"tailnet-identity only: {r}")
    admission = [
        r for r in role.get("rules") or []
        if {"admissionregistration.k8s.io", "*"} &
        set(r.get("apiGroups") or [])
    ]
    return (admission
            and "provision-write must not reach the admission rules "
            f"(cluster-gvisor-pod-policy): {admission}")
check(
    "provision-write is write-only on Secrets except the pinned "
    "tailnet-identity read, and reaches no admission rule",
    write_scope())

def generate_set():
    pol = [
        b for b in blocks if "kind: GeneratingPolicy" in b
        and "configmaps" in b and "kind: ResourceQuota" in b
    ]
    if not pol:
        return (
            "no provision GeneratingPolicy generating a ResourceQuota from a "
            "configmap provision command")
    text = "\n".join(pol)
    missing = [w for w in (
        "requests.memory: (( variables.capacity ))",
        "limits.memory: (( variables.capacity ))"
    ) if w not in text]
    if missing:
        return (
            "provision generate must size the ResourceQuota from capacity; "
            f"missing {missing}")
    kinds = re.findall(r"^\s+kind: (.*)$", text, re.M)
    return (kinds != ["Namespace", "ResourceQuota", "LimitRange"]
            and "provision generate must produce only a Namespace, a "
            f"ResourceQuota and a LimitRange, got {kinds}")
check(
    "the provision reactor generates only the tenant Namespace, a "
    "capacity-sized ResourceQuota and a LimitRange",
    generate_set())

def suspend():
    mut = "\n".join(b for b in blocks if "kind: MutatingPolicy" in b
                    and "replicas" in b and "deployments" in b)
    if not mut:
        return "no suspend MutatingPolicy scaling Deployments"
    if not re.search(r'replicas:\s*"?0"?', mut):
        return "suspend must scale tenant Deployments to zero replicas"
    ev = (yaml.safe_load(mut).get("spec") or {}).get("evaluation") or {}
    if ((ev.get("mutateExisting") or {}).get("enabled") is not True
            or ev.get("skipBackgroundRequests") is not False):
        return (
            "suspend must act on existing Deployments: want "
            "mutateExisting.enabled true and skipBackgroundRequests false, "
            f"got evaluation {ev}")
    return ("object.metadata.namespace" not in mut
            and "suspend targets must be scoped to the suspend command's "
            "own namespace")
check(
    "a suspend command scales only its own namespace's existing "
    "Deployments to zero",
    suspend())

print(f"\n{passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
PY
