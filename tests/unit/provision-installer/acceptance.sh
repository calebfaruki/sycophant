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

render() { helm template c "$CHART" -n kyverno --api-versions policies.kyverno.io/v1/ValidatingPolicy "$@"; }

generate() {
  mkdir -p "$WORK/$2-out"
  kyverno apply "$WORK/$1-policy.yaml" --resource "$HERE/$2.yaml" \
    -f "$HERE/values.yaml" -o "$WORK/$2-out" --remove-color >/dev/null 2>&1
}
for kind in tenant workspace user toolset inference channel instructions; do
  render --show-only "templates/$kind-kpol.yaml" > "$WORK/$kind-policy.yaml"
  generate "$kind" "$kind"
done
generate workspace reap-workspace
generate instructions reap-instructions
render > "$WORK/cluster.yaml"

mkdir -p "$WORK/bin"
cat > "$WORK/bin/kubectl" <<'SH'
#!/bin/sh
printf 'kubectl %s\n' "$*" >> "$CALLS"
case "$*" in
  *"get configmap"*"-o json"*) [ -n "${GET_CM_EXIT:-}" ] && exit "$GET_CM_EXIT"; _d='{}'; printf '{"metadata":{"uid":"%s","creationTimestamp":"2026-01-01T00:00:00Z"},"data":%s}' "${ORIG_UID:-$COMMAND_UID}" "${GET_CM-$_d}" ;;
  *"get configmap"*) [ -n "${GET_CM_EXIT:-}" ] && exit "$GET_CM_EXIT"; printf '%s' "${GET_CM-{\}}" ;;
  *"get deployment"*) [ -n "${GET_DEPLOY_EXIT:-}" ] && exit "$GET_DEPLOY_EXIT"; printf 'deployment.apps/ws-one\n' ;;
esac
exit 0
SH
cat > "$WORK/bin/helm" <<'SH'
#!/bin/sh
printf 'helm %s\n' "$*" >> "$CALLS"
exit "${HELM_EXIT:-0}"
SH
cat > "$WORK/bin/curl" <<'SH'
#!/bin/sh
printf 'curl %s\n' "$*" >> "$CALLS"
exit 0
SH
# BusyBox tar as the provisioner image ships it (v1.37.0), for the job's two
# calls, `-tvzf FILE` and `-xzf FILE -C DIR`: both strip a leading '/' or
# '../' with a stderr notice and exit 0; links list with ' -> '.
cat > "$WORK/bin/tar" <<SH
#!$(python3 -c 'import sys; print(sys.executable)')
SH
cat >> "$WORK/bin/tar" <<'SH'
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
                    sys.stderr.write(f"tar: removing leading '{p}' from member names\n")
                    warned.add(p)
                name = name[len(p):]
                break
        else:
            return name
for m in tf:
    name = strip(m.name)
    if mode == "t":
        kind = "d" if m.isdir() else "l" if m.issym() else "-"
        perm = "".join(c if m.mode & (1 << (8 - n)) else "-" for n, c in enumerate("rwxrwxrwx"))
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
chmod +x "$WORK/bin/kubectl" "$WORK/bin/helm" "$WORK/bin/curl" "$WORK/bin/tar"

python3 - "$WORK" "$HERE" <<'PY'
import base64, io, json, os, re, subprocess, sys, tarfile, yaml

work = sys.argv[1]
uid10 = "0a1b2c3d4e"
cases = {
    "tenant": {"command": "provision-tenant-acme", "command_ns": "sycophant-system", "release": None},
    "workspace": {"command": "provision-workspace-one", "command_ns": "tenant-acme", "release": "ws-one"},
    "user": {"command": "provision-user-0123456789", "command_ns": "tenant-acme", "release": "user-0123456789"},
    "toolset": {"command": "provision-toolset-one", "command_ns": "tenant-acme", "release": "ts-one"},
    "inference": {"command": "provision-inference-local", "command_ns": "tenant-acme", "release": "inf-local"},
    "channel": {"command": "provision-channel-app", "command_ns": "tenant-acme", "release": "ch-app"},
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
        if d.get("kind") == kind and m.get("name") == name and m.get("namespace") == ns:
            return d
    return None

def run_job(job, helm_exit, get_cm="{}", get_cm_exit="", wrong_uid=False, get_deploy_exit=""):
    c = job["spec"]["template"]["spec"]["containers"][0]
    script = c["args"][0].replace("/tmp/", work + "/scratch/")
    os.makedirs(work + "/scratch", exist_ok=True)
    calls = os.path.join(work, "calls")
    open(calls, "w").close()
    env = {e["name"]: e.get("value", "") for e in c.get("env", [])}
    import shutil
    jq_dir = os.path.dirname(shutil.which("jq") or "/usr/local/bin/jq")
    env.update(PATH=f"{work}/bin:{jq_dir}:/usr/bin:/bin", CALLS=calls, HELM_EXIT=str(helm_exit), HOME=work, GET_CM=get_cm, GET_CM_EXIT=get_cm_exit, GET_DEPLOY_EXIT=get_deploy_exit)
    # The stub always returns the command's real uid in its JSON.
    # ORIG_UID carries it; COMMAND_UID is what the Job thinks its command's uid is.
    env["ORIG_UID"] = env.get("COMMAND_UID", "")
    if wrong_uid:
        env["COMMAND_UID"] = "wrong-uid-from-a-different-grant"
    rc = subprocess.run(["sh", "-c", script], env=env, capture_output=True).returncode
    with open(calls) as fh:
        return rc, [l.split() for l in fh.read().splitlines()]

def selects(selector, labels):
    if any(labels.get(k) != v for k, v in (selector.get("matchLabels") or {}).items()):
        return False
    return all(e["operator"] == "In" and labels.get(e["key"]) in e["values"]
               for e in selector.get("matchExpressions") or [])

def reap_policy(kind):
    return next((d for d in yaml.safe_load_all(open(os.path.join(work, f"{kind}-policy.yaml"))) or []
                 if d and d.get("kind") == "DeletingPolicy" and d["metadata"]["name"] == f"reap-{kind}"), None)

def reaps(policy, docs):
    selector = policy["spec"]["matchConstraints"]["objectSelector"]
    expr = next(v["expression"] for v in policy["spec"]["variables"] if v["name"] == "reapAfter")
    annotation = re.search(r'annotations\["([^"]+)"\]', expr).group(1)
    return [f"{d['kind']}/{d['metadata']['name']}" for d in docs
            if not selects(selector, d["metadata"].get("labels") or {})
            or not (d["metadata"].get("annotations") or {}).get(annotation)]

def deletes_command(argv, command, ns):
    if argv[0] != "kubectl" or "delete" not in argv or command not in argv:
        return False
    if not any(a in ("configmap", "configmaps", "cm") for a in argv):
        return False
    return any(argv[i] in ("-n", "--namespace") and argv[i + 1] == ns for i in range(len(argv) - 1)) \
        or f"--namespace={ns}" in argv

# The per-kind install paths share one script body (the stale-command guard is
# the shared else branch), so tenant and workspace stand for every kind here.
for kind in ("tenant", "workspace"):
    c = cases[kind]
    docs = generated(kind)
    name = f"provision-{kind}-{uid10}"
    sa = find(docs, "ServiceAccount", name, "sycophant-system")
    job = find(docs, "Job", name, "sycophant-system")
    rb = find(docs, "RoleBinding", name, "tenant-acme")
    if not (sa and job and rb and job["spec"]["template"]["spec"].get("serviceAccountName") == name
            and [s.get("name") for s in rb.get("subjects", [])] == [name]):
        bad(f"{kind}: install identity named from the command uid",
            f"want ServiceAccount and Job {name} in sycophant-system and RoleBinding {name} in tenant-acme using it; generated {[(d['kind'], d['metadata'].get('name')) for d in docs]}")
        continue
    ok(f"{kind}: install identity named from the command uid")

    if rb["roleRef"] == {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "provision-write"}:
        ok(f"{kind}: install RoleBinding binds ClusterRole provision-write")
    else:
        bad(f"{kind}: install RoleBinding binds ClusterRole provision-write", f"roleRef {rb['roleRef']}")

    if kind != "tenant":
        reap = reap_policy(kind)
        miss = reaps(reap, (sa, rb)) if reap else ["no DeletingPolicy"]
        if miss:
            bad(f"reap-{kind} selects this install SA and RoleBinding", f"not reaped: {miss}")
        else:
            ok(f"reap-{kind} selects this install SA and RoleBinding")

    rc, calls = run_job(job, 0)
    helm_at = [i for i, a in enumerate(calls) if a[0] == "helm" and "upgrade" in a]
    del_at = [i for i, a in enumerate(calls) if deletes_command(a, c["command"], c["command_ns"])]
    if rc == 0 and helm_at and del_at and min(del_at) > max(helm_at):
        ok(f"{kind}: successful install deletes its command after helm")
    else:
        bad(f"{kind}: successful install deletes its command after helm",
            f"rc={rc}, calls={[' '.join(a) for a in calls]}")

    rc, calls = run_job(job, 1)
    if rc != 0 and not any(deletes_command(a, c["command"], c["command_ns"]) for a in calls):
        ok(f"{kind}: failed install leaves its command")
    else:
        bad(f"{kind}: failed install leaves its command", f"rc={rc}, calls={[' '.join(a) for a in calls]}")

    if kind != "tenant":
        # A retry that finds its command gone must not reach helm: empty
        # values would prune the release.
        name = f"{kind}: a missing command stops the install before helm"
        rc, calls = run_job(job, 0, get_cm_exit="1")
        if rc != 0 and not any(a[0] == "helm" for a in calls):
            ok(name)
        else:
            bad(name, f"rc={rc}, calls={[' '.join(a) for a in calls]}")

        name = f"{kind}: a replaced command stops the install before helm"
        rc, calls = run_job(job, 0, wrong_uid=True)
        if rc != 0 and not any(a[0] == "helm" for a in calls):
            ok(name)
        else:
            bad(name, f"rc={rc}, calls={[' '.join(a) for a in calls]}")

docs = generated("tenant")
name = f"provision-tenant-{uid10}"
rb = find(docs, "RoleBinding", name, "sycophant-system")
if rb and rb["roleRef"] == {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": name} \
        and [(s.get("kind"), s.get("name"), s.get("namespace")) for s in rb.get("subjects", [])] == [("ServiceAccount", name, "sycophant-system")]:
    ok("tenant: install ServiceAccount bound to its own command-delete Role")
else:
    bad("tenant: install ServiceAccount bound to its own command-delete Role", f"got {rb}")

role = find(docs, "Role", name, "sycophant-system")
rules = [(r.get("apiGroups"), r.get("resources"), r.get("verbs"), r.get("resourceNames")) for r in (role or {}).get("rules") or []]
if rules == [([""], ["configmaps"], ["delete"], [cases["tenant"]["command"]])]:
    ok("tenant: the install's Role may delete only its own tenant command")
else:
    bad("tenant: the install's Role may delete only its own tenant command", f"got {role}")

reap_t = next((d for d in yaml.safe_load_all(open(os.path.join(work, "tenant-policy.yaml"))) or []
               if d and d.get("kind") == "DeletingPolicy" and d["metadata"]["name"] == "reap-tenant"), None)
t_sa = find(docs, "ServiceAccount", f"provision-tenant-{uid10}", "sycophant-system")
name = "tenant: the scheduled sweep selects the install ServiceAccount, Role and RoleBinding"
if not (reap_t and t_sa):
    bad(name, "missing reap-tenant DeletingPolicy or install ServiceAccount")
else:
    cond = next(c["expression"] for c in reap_t["spec"]["conditions"] if c["name"] == "selector-matches-by-kind")
    kinds = {r for rule in reap_t["spec"]["matchConstraints"]["resourceRules"] for r in rule["resources"]}
    if f'"{t_sa["metadata"]["labels"]["app.kubernetes.io/component"]}"' in cond \
            and {"namespaces", "serviceaccounts", "rolebindings", "roles"} <= kinds:
        ok(name)
    else:
        bad(name, f"component label not in selector or kinds {kinds}")

def install_call(kind, get_cm="{}"):
    job = find(generated(kind), "Job", f"provision-{kind}-{uid10}", "sycophant-system")
    if not job:
        return None
    _, calls = run_job(job, 0, get_cm)
    return next((a for a in calls if a[0] == "helm" and "upgrade" in a), None)

job = find(generated("tenant"), "Job", f"provision-tenant-{uid10}", "sycophant-system")
argv = install_call("tenant") or []
env = {e["name"]: e.get("value", "") for e in job["spec"]["template"]["spec"]["containers"][0].get("env", [])} if job else {}
name = "tenant: install passes headscale values via -f /tmp/hs-values.yaml"
hs_file = [argv[i + 1] for i in range(len(argv) - 1) if argv[i] == "-f"]
hs_env_keys = {"HS_ENABLED", "HS_SERVER_URL", "HS_MAGIC_DNS", "HS_BASE_DOMAIN", "HS_STORAGE_CLASS", "HS_ACME_ENABLED", "HS_ACME_EMAIL"}
if job and argv and hs_file == [work + "/scratch/hs-values.yaml"] and hs_env_keys <= set(env):
    ok(name)
else:
    bad(name, f"argv={' '.join(argv)}, env_keys={sorted(env)}, hs_file={hs_file}")

taking = [k for k in ("workspace", "user", "toolset", "inference", "channel")
          if (install_call(k) or ["--take-ownership"]).count("--take-ownership")]
if taking:
    bad("capability installs never take ownership of another release's objects", f"--take-ownership on {taking}")
else:
    ok("capability installs never take ownership of another release's objects")

argv = install_call("user") or []
name = "user: install hands the command's write time to the chart"
if any(a.startswith("commandCreated=") for a in argv):
    ok(name)
else:
    bad(name, f"no commandCreated in {' '.join(argv)}")

# The user install rides the generic values path like every other kind: its
# chart consumes .Values.command, and the installer passes --set-string name=
# unconditionally.
problems = {}
for kind, short in (("tenant", None), ("inference", "local"), ("toolset", "one"), ("workspace", "one"), ("channel", "app"), ("user", "0123456789")):
    command_data = yaml.safe_load(open(os.path.join(sys.argv[2], f"{kind}.yaml")))["data"] if short else {}
    argv = install_call(kind, json.dumps(command_data)) or []
    at = argv.index("--install") + 2 if "--install" in argv else len(argv)
    chart = argv[at] if at < len(argv) else None
    sets = [argv[i + 1] for i in range(len(argv) - 1) if argv[i] in ("--set", "--set-string")]
    files = [argv[i + 1] for i in range(len(argv) - 1) if argv[i] in ("-f", "--values")]
    loaded = [yaml.safe_load(open(f)) for f in files if os.path.exists(f)]
    wrong = [w for w, bad_ in (
        ("own chart", chart != f"/charts/sycophant-{kind}"),
        ("tenant namespace.create=false", kind == "tenant" and "namespace.create=false" not in sets),
        ("release from the command name suffix", short and cases[kind]["release"] not in argv),
        ("command data", short and not any((v or {}).get("command") == command_data for v in loaded)),
        ("name=", short and f"name={short}" not in sets),
    ) if bad_]
    if wrong:
        problems[kind] = f"{wrong}: argv={' '.join(argv)}, values={loaded}"
name = "every kind's install runs its own chart under the command's release name and hands it the command data"
if problems:
    bad(name, problems)
else:
    ok(name)

# Every reap kind shares the uninstall script body; workspace stands for all.
for kind, release, prefix in (("workspace", "one", "ws"),):
    docs = generated(f"reap-{kind}")
    command = f"reap-{kind}-{release}"
    name = f"uninstall-{kind}-{uid10}"
    sa = find(docs, "ServiceAccount", name, "sycophant-system")
    job = find(docs, "Job", name, "sycophant-system")
    rb = find(docs, "RoleBinding", name, "tenant-acme")
    if sa and job and rb and job["spec"]["template"]["spec"].get("serviceAccountName") == name \
            and rb["roleRef"] == {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "provision-write"} \
            and [(s.get("kind"), s.get("name"), s.get("namespace")) for s in rb.get("subjects", [])] == [("ServiceAccount", name, "sycophant-system")]:
        ok(f"{kind}: reap command generates a uid-named uninstall identity bound to provision-write")
    else:
        bad(f"{kind}: reap command generates a uid-named uninstall identity bound to provision-write",
            f"generated {[(d['kind'], d['metadata'].get('name'), d['metadata'].get('namespace')) for d in docs]}")

    if job:
        rc, calls = run_job(job, 0)
        un_at = [i for i, a in enumerate(calls) if a[:2] == ["helm", "uninstall"] and f"{prefix}-{release}" in a
                 and any(a[j] in ("-n", "--namespace") and a[j + 1] == "tenant-acme" for j in range(len(a) - 1))]
        del_at = [i for i, a in enumerate(calls) if deletes_command(a, command, "tenant-acme")]
        if rc == 0 and un_at and del_at and min(del_at) > max(un_at) and not any(a[0] == "helm" and "upgrade" in a for a in calls):
            ok(f"{kind}: uninstall removes the release, then deletes its reap command")
        else:
            bad(f"{kind}: uninstall removes the release, then deletes its reap command", f"rc={rc}, calls={[' '.join(a) for a in calls]}")
        rc, calls = run_job(job, 1)
        if rc != 0 and not any(deletes_command(a, command, "tenant-acme") for a in calls):
            ok(f"{kind}: failed uninstall leaves its reap command")
        else:
            bad(f"{kind}: failed uninstall leaves its reap command", f"rc={rc}, calls={[' '.join(a) for a in calls]}")
    else:
        bad(f"{kind}: uninstall removes the release, then deletes its reap command", "no uninstall Job")
        bad(f"{kind}: failed uninstall leaves its reap command", "no uninstall Job")

    reap = reap_policy(kind)
    name = f"{kind}: the scheduled sweep selects the uninstall identity"
    if not (reap and sa and rb):
        bad(name, f"missing reap-{kind} DeletingPolicy or uninstall identity")
    else:
        miss = [f"{d['kind']}/{d['metadata']['name']}" for d in (sa, rb)
                if (d["metadata"].get("labels") or {}).get("app.kubernetes.io/component") != f"uninstall-{kind}"]
        miss += reaps(reap, (sa, rb))
        if miss:
            bad(name, f"{miss} not labelled uninstall-{kind} or not reaped")
        else:
            ok(name)

def archive_of(members):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz", format=tarfile.USTAR_FORMAT) as tf:
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

FOLDER = "/buckets/sycophant-instructions/system:serviceaccount:tenant-acme:ws-one/"

def store_calls(calls):
    out = []
    for i, a in enumerate(calls):
        if a[0] != "curl":
            continue
        url = next((x for x in a if "seaweedfs.sycophant-system.svc:8888" in x), "")
        method = next((a[j + 1] for j in range(len(a) - 1) if a[j] in ("-X", "--request")), "")
        out.append((i, method.upper(), url))
    return out

def restarts(calls):
    return [i for i, a in enumerate(calls) if a[0] == "kubectl" and "rollout" in a and "restart" in a
            and "deployment/ws-one" in a
            and any(a[j] in ("-n", "--namespace") and a[j + 1] == "tenant-acme" for j in range(len(a) - 1))]

def show(calls):
    return [" ".join(a) for a in calls]

inst = find(generated("instructions"), "Job", f"provision-instructions-{uid10}", "sycophant-system")
inst_rb = find(generated("instructions"), "RoleBinding", f"provision-instructions-{uid10}", "tenant-acme")
good = archive_of([("AGENTS.md", "file", b"You are the probe.\n"), ("docs", "dir", None), ("docs/deep.md", "file", b"deep\n")])
name = "instructions: an install replaces the folder from the archive, restarts the workspace, then deletes its command"
if not inst:
    bad(name, f"no Job provision-instructions-{uid10}; generated {[(d['kind'], d['metadata'].get('name')) for d in generated('instructions')]}")
else:
    rc, calls = run_job(inst, 0, json.dumps({"archive": good}))
    store = store_calls(calls)
    listed = [i for i, a in enumerate(calls) if a[0] == "tar" and "t" in a[1]]
    wipe = [i for i, m, u in store if m == "DELETE" and u.endswith(FOLDER + "?recursive=true")]
    puts = {u.split(FOLDER, 1)[-1]: i for i, m, u in store if m == "PUT" and FOLDER in u}
    rs = restarts(calls)
    dels = [i for i, a in enumerate(calls) if deletes_command(a, "provision-instructions-one", "tenant-acme")]
    if rc == 0 and listed and store and min(listed) < min(i for i, _, _ in store) and wipe \
            and set(puts) == {"AGENTS.md", "docs/deep.md"} and min(puts.values()) > max(wipe) \
            and rs and min(rs) > max(puts.values()) and dels and min(dels) > max(rs) \
            and not any(a[0] == "helm" for a in calls):
        ok(name)
    else:
        bad(name, f"rc={rc}, calls={show(calls)}")

    name = "instructions: a failed workspace lookup fails the job before the restart and leaves its command"
    rc, calls = run_job(inst, 0, json.dumps({"archive": good}), get_deploy_exit="1")
    if rc != 0 and not restarts(calls) \
            and not any(deletes_command(a, "provision-instructions-one", "tenant-acme") for a in calls):
        ok(name)
    else:
        bad(name, f"rc={rc}, calls={show(calls)}")

    for label, members in (
        ("a symlink", [("AGENTS.md", "file", b"x\n"), ("ln", "symlink", "/etc/passwd")]),
        ("a path outside its folder", [("AGENTS.md", "file", b"x\n"), ("../evil.md", "file", b"x\n")]),
        ("an unsafe name", [("AGENTS.md", "file", b"x\n"), ("a?b.md", "file", b"x\n")]),
    ):
        name = f"instructions: an archive holding {label} fails before any store call and leaves its command"
        rc, calls = run_job(inst, 0, json.dumps({"archive": archive_of(members)}))
        if rc != 0 and not store_calls(calls) and not restarts(calls) \
                and not any(deletes_command(a, "provision-instructions-one", "tenant-acme") for a in calls):
            ok(name)
        else:
            bad(name, f"rc={rc}, calls={show(calls)}")

cluster_text = open(os.path.join(work, "cluster.yaml")).read()
cluster = [d for d in yaml.safe_load_all(cluster_text) if d]

name = "instructions: the install binds a narrow ClusterRole, not provision-write"
role_name = (inst_rb or {}).get("roleRef", {}).get("name")
role = next((d for d in cluster if d.get("kind") == "ClusterRole"
             and d["metadata"]["name"] == role_name), None) if role_name else None
grants = sorted((g, r, v) for rule in (role or {}).get("rules") or [] for g in rule.get("apiGroups", [])
                for r in rule.get("resources", []) for v in rule.get("verbs", []))
want = sorted([("apps", "deployments", "get"), ("apps", "deployments", "patch"),
               ("", "configmaps", "get"), ("", "configmaps", "delete")])
if role_name and role_name != "provision-write" and grants == want:
    ok(name)
else:
    bad(name, f"roleRef={role_name}, rules={grants}")

reap_inst = find(generated("reap-instructions"), "Job", f"uninstall-instructions-{uid10}", "sycophant-system")
name = "instructions: a reap removes the folder, restarts the workspace, then deletes its reap command"
if not reap_inst:
    bad(name, f"no Job uninstall-instructions-{uid10}; generated {[(d['kind'], d['metadata'].get('name')) for d in generated('reap-instructions')]}")
else:
    rc, calls = run_job(reap_inst, 0)
    wipe = [i for i, m, u in store_calls(calls) if m == "DELETE" and u.endswith(FOLDER + "?recursive=true")]
    rs = restarts(calls)
    dels = [i for i, a in enumerate(calls) if deletes_command(a, "reap-instructions-one", "tenant-acme")]
    if rc == 0 and wipe and rs and min(rs) > max(wipe) and dels and min(dels) > max(rs):
        ok(name)
    else:
        bad(name, f"rc={rc}, calls={show(calls)}")

blocks = re.split(r"\n---\n", cluster_text)

def check(name, why):
    if why:
        bad(name, why)
    else:
        ok(name)

def identity_minimal():
    roles = [d for d in cluster if d.get("kind") == "ClusterRole" and d["metadata"]["name"] == "provision-identity"]
    if len(roles) != 1:
        return f"provision-identity ClusterRole must render exactly once, got {len(roles)}"
    role = roles[0]
    if role.get("rules") != []:
        return f"provision-identity must grant nothing, got {role.get('rules')}"
    if "aggregationRule" in role:
        return "provision-identity must not aggregate rules from other ClusterRoles"
    agg = [k for k in role["metadata"].get("labels") or {} if "aggregate-to-" in k]
    return agg and f"provision-identity must not aggregate into other ClusterRoles: {agg}"
check("the provision-identity ClusterRole has no rules and no aggregation", identity_minimal())

# provision-write is static (no aggregationRule), so what it can do in a
# tenant namespace is exactly the rendered rules.
def write_scope():
    role = next((d for d in cluster if d.get("kind") == "ClusterRole" and d["metadata"]["name"] == "provision-write"), None)
    if role is None:
        return "no provision-write ClusterRole rendered"
    if "aggregationRule" in role:
        return "provision-write must not aggregate rules from other ClusterRoles"
    reads = {"get", "list", "watch"}
    rules = [r for r in role.get("rules") or [] if "secrets" in r.get("resources", [])]
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
            if sorted(r["resourceNames"]) != ["tailnet-identity"] or set(r["verbs"]) - {"get"}:
                return f"scoped Secret read must be get on tailnet-identity only: {r}"
    admission = [r for r in role.get("rules") or [] if {"admissionregistration.k8s.io", "*"} & set(r.get("apiGroups") or [])]
    return admission and f"provision-write must not reach the admission rules (cluster-gvisor-pod-policy): {admission}"
check("provision-write is write-only on Secrets except the pinned tailnet-identity read, and reaches no admission rule", write_scope())

def generate_set():
    pol = [b for b in blocks if "kind: GeneratingPolicy" in b and "configmaps" in b and "kind: ResourceQuota" in b]
    if not pol:
        return "no provision GeneratingPolicy generating a ResourceQuota from a configmap provision command"
    text = "\n".join(pol)
    missing = [w for w in ("kind: Namespace", "kind: ResourceQuota", "capacity", "kind: LimitRange") if w not in text]
    if missing:
        return f"provision generate must produce the tenant Namespace, a ResourceQuota sized from capacity and a LimitRange; missing {missing}"
    if "kind: RoleBinding" in text:
        return "provision generate must produce no RoleBinding"
    return "authority" in text.lower() and "a provision command carries no authority field; selection is identity, not a command field"
check("the provision reactor generates only the tenant Namespace, a capacity-sized ResourceQuota and a LimitRange", generate_set())

def suspend():
    mut = "\n".join(b for b in blocks if "kind: MutatingPolicy" in b and "replicas" in b and "deployments" in b)
    if not mut:
        return "no suspend MutatingPolicy scaling Deployments"
    if not re.search(r'replicas:\s*"?0"?', mut):
        return "suspend must scale tenant Deployments to zero replicas"
    if "mutateExisting" not in mut:
        return "suspend acts on Deployments that already exist"
    return "object.metadata.namespace" not in mut and "suspend targets must be scoped to the suspend command's own namespace"
check("a suspend command scales only its own namespace's existing Deployments to zero", suspend())

print(f"\n{passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
PY
