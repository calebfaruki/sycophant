use std::io::{self, BufRead, IsTerminal};
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::cli::IngressUp;
use crate::commands::command::find_tenant_ns;
use crate::commands::common::{ok, step, warn};
use crate::commands::setup;
use crate::interrupt::{self, UndoList};
use crate::runner::{run_output, run_passthrough, run_passthrough_own_group, run_stdin};
use crate::scope::Scope;

const NAMESPACE: &str = "sycophant-ingress";
const RELEASE: &str = "sycophant-ingress";
const NODEPORT: u16 = 30444;
const VPN_SERVICE: &str = "headscale";
const VPN_PORT: u16 = 8080;
const VPN_COMPONENT: &str = "headscale";
const CERT_OBTAINED: &str = "certificate obtained successfully";
const CERT_FAILED: &str = "could not get certificate from issuer";
const CERT_CAUSES: &str = "Likely causes, each with its check:\n  \
    - the router forward is missing, or points at the wrong LAN IP: compare it with the LAN IPv4 in the steps above\n  \
    - the Docker Desktop setting is off: Settings > Advanced > \"Allow privileged port forwarding to external machines\"\n  \
    - the A record is not this home: compare it with the WAN IP on the router's status page\n  \
    - the name has an IPv6 address (AAAA): check the name's records at your registrar, and remove it\n  \
    - CGNAT: the router's WAN IP is in 100.64.0.0/10, so this home has no public IP\n  \
    - double NAT: the router's WAN IP is 10.x, 172.16-31.x or 192.168.x; forward 443 on the upstream box too, if you control it\n  \
    - the ISP blocks inbound 443: check the ISP's terms, or ask it";
const ROOTLESS_DOCKER: &str =
    "on Linux with rootless Docker, set `net.ipv4.ip_unprivileged_port_start` to 443 or less";
const OPERATOR_ITEMS: &str = "Ingress removed. These are yours to remove by hand:\n  \
    - on the router, the forward of TCP port 443 to this machine\n  \
    - on macOS with Docker Desktop, if nothing else needs it: Settings > Advanced > \"Allow privileged port forwarding to external machines\"\n  \
    - at the registrar, if the name is no longer used: its A record, and the CAA record if you added it\n\
    A later `sycoadm ingress up` requests a new certificate.";

struct Opts {
    domain: String,
    vpn_tenant: String,
    cert_deadline: Duration,
}

trait Seam: Send + Sync {
    fn cluster_exists(&self) -> Result<bool, String>;
    fn release_exists(&self) -> Result<bool, String>;
    fn namespace_exists(&self) -> Result<bool, String>;
    /// Whether k3d's loadbalancer publishes host port 443.
    fn k3d_port_exists(&self) -> Result<bool, String>;
    fn resolve(&self, name: &str) -> Vec<IpAddr>;
    fn lan_ip(&self, toward: Ipv4Addr) -> Result<Ipv4Addr, String>;
    fn wait_for_operator(&self, steps: &str) -> Result<(), String>;
    fn k3d_port_add(&self) -> Result<(), String>;
    fn k3d_port_delete(&self) -> Result<(), String>;
    fn apply_namespace(&self) -> Result<(), String>;
    fn delete_namespace(&self) -> Result<(), String>;
    fn helm_install(&self, values: &str) -> Result<(), String>;
    fn helm_uninstall(&self) -> Result<(), String>;
    fn caddy_logs(&self) -> String;
}

pub(crate) fn up(cmd: IngressUp) -> Result<(), String> {
    let vpn_tenant = find_tenant_ns(&cmd.vpn_tenant, None)?;
    let opts = Opts {
        domain: cmd.domain,
        vpn_tenant,
        cert_deadline: Duration::from_secs(300),
    };
    let scope = Scope::global()?;
    crate::sync::extract_assets(&scope)?;
    let seam = Arc::new(Real {
        chart: scope.chart_dir(RELEASE),
    });
    run(seam, &interrupt::UNDOS, &opts)
}

pub(crate) fn down() -> Result<(), String> {
    let seam = Real {
        chart: PathBuf::new(),
    };
    eprintln!("{}", run_down(&seam)?);
    Ok(())
}

/// In reverse of `up`. No undo: a re-run finishes whatever a failed step left.
fn run_down(seam: &dyn Seam) -> Result<String, String> {
    step("Checking what is present");
    if !seam.cluster_exists()? {
        return Ok(format!("no ingress: no k3d cluster `{}`", setup::CLUSTER));
    }
    let release = seam.release_exists()?;
    let namespace = seam.namespace_exists()?;
    let port = seam.k3d_port_exists()?;
    if !release && !namespace && !port {
        return Ok("no ingress on this cluster".to_string());
    }
    let failed = |what: &str, e: String| {
        format!("could not remove {what}: {e}\n  `sycoadm ingress down` again finishes the rest")
    };
    if release {
        step("Uninstalling Caddy");
        seam.helm_uninstall()
            .map_err(|e| failed("the release", e))?;
    }
    if namespace {
        step("Deleting the ingress namespace");
        seam.delete_namespace()
            .map_err(|e| failed("the namespace", e))?;
    }
    if port {
        step("Removing host port 443 from k3d");
        warn("k3d marks `cluster edit` experimental");
        seam.k3d_port_delete()
            .map_err(|e| failed("the k3d port 443", e))?;
    }
    Ok(OPERATOR_ITEMS.to_string())
}

fn run(seam: Arc<dyn Seam>, undos: &UndoList, opts: &Opts) -> Result<(), String> {
    let lan = preflight(seam.as_ref(), opts)?;
    let domain = &opts.domain;
    let values = values(opts);
    seam.wait_for_operator(&operator_steps(&domain, lan))?;
    match changes(&seam, undos, &domain, &values, opts.cert_deadline) {
        Ok(()) => {
            undos.clear();
            ok(&format!("ingress serving https://{domain}"));
            eprintln!(
                "  Devices at home may need a local DNS or hosts entry from {domain} to {lan}:\n  \
                 some routers do not send a request for the home's own public IP back inside.\n  \
                 Set `headscale.serverUrl` to https://{domain} in the --vpn-tenant tenant's values,\n  \
                 then re-run `sycoadm tenant up -f <file>`, so devices join through it."
            );
            Ok(())
        }
        Err(e) => {
            undos.drain();
            if interrupt::interrupted() {
                std::process::exit(130);
            }
            Err(e)
        }
    }
}

fn preflight(seam: &dyn Seam, opts: &Opts) -> Result<Ipv4Addr, String> {
    step("Checking before any change");
    if !seam.cluster_exists()? {
        return Err(format!(
            "no k3d cluster `{}`; run `sycoadm setup` first",
            setup::CLUSTER
        ));
    }
    if seam.release_exists()? || seam.namespace_exists()? || seam.k3d_port_exists()? {
        return Err(
            "ingress already set up, or left over from an earlier run; `sycoadm ingress down` removes it"
                .to_string(),
        );
    }
    let domain = &opts.domain;
    if !is_dns_name(&domain) {
        return Err(format!(
            "{domain:?} is not a lowercase DNS name like ingress.example.com; a bare IP is unsafe with Tailscale clients"
        ));
    }
    let resolved = seam.resolve(&domain);
    if resolved.iter().any(IpAddr::is_ipv6) {
        return Err(format!(
            "remove the AAAA record for {domain}: this ingress serves IPv4 only; IPv6 would need a separate router firewall rule, and a stray AAAA can send validation and clients to another server"
        ));
    }
    let Some(a) = resolved.iter().find_map(|ip| match ip {
        IpAddr::V4(v4) => Some(*v4),
        IpAddr::V6(_) => None,
    }) else {
        return Err(format!(
            "{domain} has no IPv4 address: add an A record for {domain} pointing at this home's public IPv4"
        ));
    };
    let lan = seam.lan_ip(a)?;
    ok(&format!(
        "{domain} has an A record ({a}) and no AAAA record"
    ));
    Ok(lan)
}

fn operator_steps(domain: &str, lan: Ipv4Addr) -> String {
    format!(
        "Do these steps by hand, then press Enter:\n  \
         1. On the router, forward TCP port 443 to {lan} port 443.\n     \
            Keep that address for this machine: a DHCP reservation in the router, or a static address.\n  \
         2. On macOS with Docker Desktop: turn on Settings > Advanced >\n     \
            \"Allow privileged port forwarding to external machines\".\n     \
            It applies to every container's ports below 1024, not only 443.\n  \
         3. Optional: add this record at your registrar:\n       \
              {}\n     \
            It lets only Let's Encrypt, and only through port 443, issue certificates for the name.\n     \
            Without it, anyone who receives this home's port 80 (another device behind a forward,\n     \
            or a port a device opened on the router itself) can get a certificate for the name.",
        caa_record(domain)
    )
}

fn wait_for_enter(is_terminal: bool, input: &mut impl BufRead) -> Result<(), String> {
    if !is_terminal {
        return Ok(());
    }
    let mut line = String::new();
    input
        .read_line(&mut line)
        .map_err(|e| format!("could not read stdin: {e}"))?;
    Ok(())
}

fn changes(
    seam: &Arc<dyn Seam>,
    undos: &UndoList,
    domain: &str,
    values: &str,
    cert_deadline: Duration,
) -> Result<(), String> {
    step("Publishing host port 443 from k3d");
    warn("k3d marks `cluster edit` experimental");
    {
        let mut held = undos.hold();
        seam.k3d_port_add().map_err(|e| {
            format!("k3d could not publish host port 443: {e}\n  {ROOTLESS_DOCKER}")
        })?;
        let s = seam.clone();
        held.record(
            "the k3d port 443",
            &format!(
                "k3d cluster edit {} --port-delete {}",
                setup::CLUSTER,
                k3d_port()
            ),
            move || s.k3d_port_delete(),
        );
    }
    step("Deploying Caddy");
    {
        let mut held = undos.hold();
        let s = seam.clone();
        held.record(
            "the namespace",
            &format!("kubectl delete namespace {NAMESPACE} --ignore-not-found --wait"),
            move || s.delete_namespace(),
        );
        seam.apply_namespace()?;
    }
    {
        let mut held = undos.hold();
        let s = seam.clone();
        held.record(
            "the release",
            &format!("helm uninstall {RELEASE} -n {NAMESPACE} --ignore-not-found --wait"),
            move || s.helm_uninstall(),
        );
        seam.helm_install(values)?;
    }
    step(&format!("Waiting for the certificate for {domain}"));
    watch_certificate(seam.as_ref(), cert_deadline).map_err(|e| format!("{e}\n{CERT_CAUSES}"))
}

fn watch_certificate(seam: &dyn Seam, deadline: Duration) -> Result<(), String> {
    let end = Instant::now() + deadline;
    loop {
        let logs = seam.caddy_logs();
        // The first failure ends the watch; Caddy would retry.
        if let Some(line) = logs.lines().find(|l| l.contains(CERT_FAILED)) {
            return Err(format!("Caddy could not get the certificate:\n  {line}"));
        }
        if logs.contains(CERT_OBTAINED) {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(format!("no certificate after {}s", deadline.as_secs()));
        }
        sleep(Duration::from_secs(2));
    }
}

fn values(opts: &Opts) -> String {
    serde_json::json!({
        "domain": opts.domain,
        "nodePort": NODEPORT,
        "vpn": {
            "namespace": opts.vpn_tenant,
            "service": VPN_SERVICE,
            "port": VPN_PORT,
            "component": VPN_COMPONENT,
        },
    })
    .to_string()
}

fn is_dns_name(name: &str) -> bool {
    let labels: Vec<&str> = name.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    labels.len() >= 2
        && labels.iter().all(label_ok)
        && labels
            .last()
            .is_some_and(|l| l.starts_with(|c: char| c.is_ascii_lowercase()))
}

/// One `validationmethods` parameter: a second one is unsatisfiable.
fn caa_record(domain: &str) -> String {
    format!("{domain}. CAA 0 issue \"letsencrypt.org; validationmethods=tls-alpn-01\"")
}

fn k3d_port() -> String {
    format!("443:{NODEPORT}@loadbalancer")
}

/// `docker port` prints one `<container port>/tcp -> <host addr>:<port>` line
/// per binding.
fn publishes_host_443(ports: &str) -> bool {
    let container = format!("{NODEPORT}/tcp");
    ports.lines().any(|l| {
        l.split_once(" -> ")
            .is_some_and(|(c, host)| c.trim() == container && host.trim().ends_with(":443"))
    })
}

/// Only a real "not found" counts as absent: helm's `release: not found` or
/// the API server's `(NotFound)`. Any other failure (cluster stopped, API not
/// answering, no such kube context) is an error naming `what`.
fn presence(what: &str, result: Result<String, String>) -> Result<bool, String> {
    match result {
        Ok(_) => Ok(true),
        Err(e) if e.contains("release: not found") || e.contains("(NotFound)") => Ok(false),
        Err(e) => Err(format!("could not read {what}: {}", e.trim())),
    }
}

/// `state` is `docker inspect -f {{.State.Running}}` of the loadbalancer. A
/// stopped one publishes nothing, so `docker port` alone would read "no port".
fn serverlb_running(state: &str) -> Result<(), String> {
    if state.trim() == "true" {
        return Ok(());
    }
    Err(format!(
        "k3d-{0}-serverlb is not running; start it with `k3d cluster start {0}`",
        setup::CLUSTER
    ))
}

struct Real {
    chart: PathBuf,
}

impl Seam for Real {
    fn cluster_exists(&self) -> Result<bool, String> {
        let list = run_output("k3d", &["cluster", "list", "-o", "json"])?;
        Ok(setup::cluster_exists(&list, setup::CLUSTER))
    }

    fn release_exists(&self) -> Result<bool, String> {
        presence(
            "the release",
            run_output("helm", &["status", RELEASE, "-n", NAMESPACE]),
        )
    }

    fn namespace_exists(&self) -> Result<bool, String> {
        presence(
            "the namespace",
            run_output("kubectl", &["get", "namespace", NAMESPACE]),
        )
    }

    /// `docker port` shows what the loadbalancer publishes now, so it reads
    /// the outcome of `--port-add` rather than k3d's own records.
    fn k3d_port_exists(&self) -> Result<bool, String> {
        let lb = format!("k3d-{}-serverlb", setup::CLUSTER);
        let unreadable = |e: String| format!("could not read the k3d port 443: {}", e.trim());
        let state = run_output("docker", &["inspect", "-f", "{{.State.Running}}", &lb])
            .map_err(unreadable)?;
        serverlb_running(&state).map_err(unreadable)?;
        let ports = run_output("docker", &["port", &lb]).map_err(unreadable)?;
        Ok(publishes_host_443(&ports))
    }

    fn resolve(&self, name: &str) -> Vec<IpAddr> {
        // NXDOMAIN is an error from getaddrinfo: read it as no address.
        (name, 443)
            .to_socket_addrs()
            .map(|addrs| addrs.map(|a| a.ip()).collect())
            .unwrap_or_default()
    }

    /// The name's IPv4 is reached through the default route, so a UDP socket
    /// connected toward it picks this host's LAN IPv4; connecting sends nothing.
    fn lan_ip(&self, toward: Ipv4Addr) -> Result<Ipv4Addr, String> {
        let failed =
            |e: io::Error| format!("could not find this host's LAN IPv4 toward {toward}: {e}");
        let socket = UdpSocket::bind("0.0.0.0:0").map_err(failed)?;
        socket.connect((toward, 443)).map_err(failed)?;
        match socket.local_addr().map_err(failed)?.ip() {
            IpAddr::V4(ip) => Ok(ip),
            IpAddr::V6(ip) => Err(format!("this host's address toward {toward} is IPv6 {ip}")),
        }
    }

    fn wait_for_operator(&self, steps: &str) -> Result<(), String> {
        eprintln!("{steps}");
        let stdin = io::stdin();
        wait_for_enter(stdin.is_terminal(), &mut stdin.lock())
    }

    fn k3d_port_add(&self) -> Result<(), String> {
        run_passthrough_own_group(
            "k3d",
            &["cluster", "edit", setup::CLUSTER, "--port-add", &k3d_port()],
        )
    }

    fn k3d_port_delete(&self) -> Result<(), String> {
        run_passthrough_own_group(
            "k3d",
            &[
                "cluster",
                "edit",
                setup::CLUSTER,
                "--port-delete",
                &k3d_port(),
            ],
        )
    }

    fn apply_namespace(&self) -> Result<(), String> {
        let manifest = self.chart.join("ingress-ns.yaml");
        run_passthrough(
            "kubectl",
            &["apply", "-f", manifest.to_string_lossy().as_ref()],
        )
    }

    fn delete_namespace(&self) -> Result<(), String> {
        run_passthrough_own_group(
            "kubectl",
            &[
                "delete",
                "namespace",
                NAMESPACE,
                "--ignore-not-found",
                "--wait",
            ],
        )
    }

    fn helm_install(&self, values: &str) -> Result<(), String> {
        let chart = self.chart.to_string_lossy().into_owned();
        run_stdin(
            "helm",
            &[
                "upgrade",
                "--install",
                RELEASE,
                &chart,
                "-n",
                NAMESPACE,
                "-f",
                "-",
                "--wait",
                "--timeout=5m",
            ],
            values,
        )
    }

    fn helm_uninstall(&self) -> Result<(), String> {
        run_passthrough_own_group(
            "helm",
            &[
                "uninstall",
                RELEASE,
                "-n",
                NAMESPACE,
                "--ignore-not-found",
                "--wait",
            ],
        )
    }

    fn caddy_logs(&self) -> String {
        run_output(
            "kubectl",
            &["logs", "-n", NAMESPACE, "deploy/caddy", "--tail=-1"],
        )
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::io::Cursor;
    use std::net::Ipv6Addr;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::Mutex;

    const DOMAIN: &str = "ingress.example.com";
    const A_RECORD: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);
    const LAN_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);
    const LE_ERROR: &str = "HTTP 400 urn:ietf:params:acme:error:connection - 203.0.113.7: Timeout during connect (likely firewall problem)";
    const CERT_OK: &str = r#"{"level":"info","ts":1793300000.1,"logger":"tls.obtain","msg":"certificate obtained successfully","identifier":"ingress.example.com","issuer":"acme-v02.api.letsencrypt.org-directory"}"#;
    const LOG_FAILED: &str = r#"{"level":"error","ts":1793300000.1,"logger":"tls.obtain","msg":"could not get certificate from issuer","identifier":"ingress.example.com","issuer":"acme-v02.api.letsencrypt.org-directory","error":"HTTP 400 urn:ietf:params:acme:error:connection - 203.0.113.7: Timeout during connect (likely firewall problem)"}"#;
    const CERT_PENDING: &str = r#"{"level":"info","ts":1793300000.1,"logger":"tls.obtain","msg":"obtaining certificate","identifier":"ingress.example.com"}"#;
    const CHANGES: [&str; 6] = [
        "k3d_port_add",
        "apply_namespace",
        "helm_install",
        "k3d_port_delete",
        "delete_namespace",
        "helm_uninstall",
    ];
    /// The rootless-Docker line belongs to the k3d port failure, not here.
    const CAUSES: [&str; 7] = [
        "wrong LAN IP",
        "Allow privileged port forwarding to external machines",
        "A record",
        "IPv6 address (AAAA)",
        "100.64.0.0/10",
        "double NAT",
        "blocks inbound 443",
    ];

    struct Fake {
        calls: Mutex<Vec<&'static str>>,
        live: Mutex<BTreeSet<&'static str>>,
        cluster: bool,
        resolved: Vec<IpAddr>,
        lan_toward: Mutex<Option<Ipv4Addr>>,
        steps: Mutex<Option<String>>,
        fail: Option<&'static str>,
        /// The call at which Ctrl-C arrives: it signals the handler thread
        /// and fails, as a child killed by SIGINT does.
        interrupt: Mutex<Option<(&'static str, Sender<()>)>>,
        logs: &'static str,
    }

    impl Fake {
        fn healthy() -> Self {
            Fake {
                calls: Mutex::new(Vec::new()),
                live: Mutex::new(BTreeSet::new()),
                cluster: true,
                resolved: vec![IpAddr::V4(A_RECORD)],
                lan_toward: Mutex::new(None),
                steps: Mutex::new(None),
                fail: None,
                interrupt: Mutex::new(None),
                logs: CERT_OK,
            }
        }

        /// A cluster where `things` ("release", "namespace", "k3d:443") are
        /// already present, as a successful or partial `up` leaves them.
        fn with_live(things: &[&'static str]) -> Self {
            Fake {
                live: Mutex::new(things.iter().copied().collect()),
                ..Fake::healthy()
            }
        }

        fn call(&self, name: &'static str) -> Result<(), String> {
            self.calls.lock().unwrap().push(name);
            if let Some((at, tx)) = self.interrupt.lock().unwrap().as_ref() {
                if *at == name {
                    tx.send(()).unwrap();
                    return Err(format!("{name} killed by SIGINT"));
                }
            }
            match self.fail {
                Some(f) if f == name => Err(format!("{name} broke")),
                _ => Ok(()),
            }
        }

        fn steps(&self) -> String {
            self.steps
                .lock()
                .unwrap()
                .clone()
                .expect("no steps printed")
        }

        fn create(&self, name: &'static str, thing: &'static str) -> Result<(), String> {
            self.call(name)?;
            self.live.lock().unwrap().insert(thing);
            Ok(())
        }

        fn remove(&self, name: &'static str, thing: &'static str) -> Result<(), String> {
            self.call(name)?;
            self.live.lock().unwrap().remove(thing);
            Ok(())
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn changes(&self) -> Vec<&'static str> {
            self.calls()
                .into_iter()
                .filter(|c| CHANGES.contains(c))
                .collect()
        }

        fn live(&self) -> BTreeSet<&'static str> {
            self.live.lock().unwrap().clone()
        }
    }

    impl Seam for Fake {
        fn cluster_exists(&self) -> Result<bool, String> {
            self.call("cluster_exists").map(|_| self.cluster)
        }
        fn release_exists(&self) -> Result<bool, String> {
            self.call("release_exists")
                .map(|_| self.live().contains("release"))
        }
        fn namespace_exists(&self) -> Result<bool, String> {
            self.call("namespace_exists")
                .map(|_| self.live().contains("namespace"))
        }
        fn k3d_port_exists(&self) -> Result<bool, String> {
            self.call("k3d_port_exists")
                .map(|_| self.live().contains("k3d:443"))
        }
        fn resolve(&self, name: &str) -> Vec<IpAddr> {
            assert_eq!(name, DOMAIN);
            let _ = self.call("resolve");
            self.resolved.clone()
        }
        fn lan_ip(&self, toward: Ipv4Addr) -> Result<Ipv4Addr, String> {
            self.call("lan_ip")?;
            *self.lan_toward.lock().unwrap() = Some(toward);
            Ok(LAN_IP)
        }
        fn wait_for_operator(&self, steps: &str) -> Result<(), String> {
            *self.steps.lock().unwrap() = Some(steps.to_string());
            self.call("wait_for_operator")
        }
        fn k3d_port_add(&self) -> Result<(), String> {
            self.create("k3d_port_add", "k3d:443")
        }
        fn k3d_port_delete(&self) -> Result<(), String> {
            self.remove("k3d_port_delete", "k3d:443")
        }
        fn apply_namespace(&self) -> Result<(), String> {
            self.create("apply_namespace", "namespace")
        }
        fn delete_namespace(&self) -> Result<(), String> {
            self.remove("delete_namespace", "namespace")
        }
        fn helm_install(&self, _values: &str) -> Result<(), String> {
            self.create("helm_install", "release")
        }
        fn helm_uninstall(&self) -> Result<(), String> {
            self.remove("helm_uninstall", "release")
        }
        fn caddy_logs(&self) -> String {
            let _ = self.call("caddy_logs");
            self.logs.to_string()
        }
    }

    fn up(fake: &Arc<Fake>, undos: &UndoList, cert_deadline: Duration) -> Result<(), String> {
        run(
            fake.clone(),
            undos,
            &Opts {
                domain: DOMAIN.to_string(),
                vpn_tenant: "acme".to_string(),
                cert_deadline,
            },
        )
    }

    #[test]
    fn no_cluster_stops_before_any_other_call() {
        let fake = Arc::new(Fake {
            cluster: false,
            ..Fake::healthy()
        });
        let err = up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap_err();
        assert!(err.contains("run `sycoadm setup` first"), "{err}");
        assert_eq!(fake.calls(), ["cluster_exists"]);
    }

    #[test]
    fn an_existing_ingress_stops_before_any_change_or_dns_lookup() {
        for left in [
            &["release"][..],
            &["namespace"],
            &["k3d:443"],
            &["release", "namespace"],
            &["namespace", "k3d:443"],
        ] {
            let fake = Arc::new(Fake::with_live(left));
            let err = up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap_err();
            assert!(
                err.contains("ingress already set up, or left over from an earlier run"),
                "{left:?}: {err}"
            );
            let calls = fake.calls();
            assert_eq!(calls[0], "cluster_exists", "{left:?}");
            assert!(
                calls[1..].iter().all(|c| [
                    "release_exists",
                    "namespace_exists",
                    "k3d_port_exists"
                ]
                .contains(c)),
                "{left:?}: {calls:?}"
            );
        }
    }

    #[test]
    fn up_with_a_failing_presence_read_changes_nothing_and_stops_with_the_error() {
        for read in ["release_exists", "namespace_exists", "k3d_port_exists"] {
            let fake = Arc::new(Fake {
                fail: Some(read),
                ..Fake::healthy()
            });
            let err = up(&fake, &UndoList::new(), Duration::from_secs(5))
                .expect_err(&format!("{read}: up went on past a failing read"));
            assert!(err.contains(&format!("{read} broke")), "{read}: {err}");
            assert!(!fake.calls().contains(&"wait_for_operator"), "{read}");
            assert_eq!(fake.changes(), Vec::<&str>::new(), "{read}");
        }
    }

    #[test]
    fn a_name_with_no_a_record_stops_before_any_change_and_asks_for_one() {
        let fake = Arc::new(Fake {
            resolved: Vec::new(),
            ..Fake::healthy()
        });
        let err = up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap_err();
        assert!(
            err.contains("add an A record for ingress.example.com"),
            "{err}"
        );
        assert!(err.contains("public IPv4"), "{err}");
        assert!(!fake.calls().contains(&"wait_for_operator"), "{err}");
        assert_eq!(fake.changes(), Vec::<&str>::new(), "{err}");
    }

    #[test]
    fn a_name_with_an_aaaa_record_stops_before_any_change_and_says_why_to_remove_it() {
        let fake = Arc::new(Fake {
            resolved: vec![
                IpAddr::V4(A_RECORD),
                IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7)),
            ],
            ..Fake::healthy()
        });
        let err = up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap_err();
        assert!(
            err.contains("remove the AAAA record for ingress.example.com"),
            "{err}"
        );
        assert!(err.contains("IPv4 only"), "{err}");
        assert!(err.contains("router firewall rule"), "{err}");
        assert!(err.contains("another server"), "{err}");
        assert!(!fake.calls().contains(&"wait_for_operator"), "{err}");
        assert_eq!(fake.changes(), Vec::<&str>::new());
    }

    #[test]
    fn any_a_record_is_accepted_with_no_public_ip_or_router_check() {
        for a in [Ipv4Addr::new(198, 51, 100, 9), LAN_IP] {
            let fake = Arc::new(Fake {
                resolved: vec![IpAddr::V4(a)],
                ..Fake::healthy()
            });
            up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap();
            assert_eq!(
                fake.calls(),
                [
                    "cluster_exists",
                    "release_exists",
                    "namespace_exists",
                    "k3d_port_exists",
                    "resolve",
                    "lan_ip",
                    "wait_for_operator",
                    "k3d_port_add",
                    "apply_namespace",
                    "helm_install",
                    "caddy_logs",
                ],
                "{a}"
            );
        }
    }

    #[test]
    fn the_lan_ip_is_found_toward_the_a_record_address() {
        let fake = Arc::new(Fake::healthy());
        up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap();
        assert_eq!(*fake.lan_toward.lock().unwrap(), Some(A_RECORD));
    }

    #[test]
    fn printed_steps_name_the_forward_and_the_kept_lan_address() {
        let fake = Arc::new(Fake::healthy());
        up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap();
        let steps = fake.steps();
        let forward = steps
            .lines()
            .find(|l| l.contains("192.168.1.20"))
            .unwrap_or_else(|| panic!("no line names the LAN IPv4:\n{steps}"));
        assert!(forward.to_lowercase().contains("forward"), "{forward}");
        assert!(forward.contains("TCP"), "{forward}");
        assert!(forward.contains("443"), "{forward}");
        assert!(steps.contains("DHCP reservation"), "{steps}");
        assert!(steps.contains("static address"), "{steps}");
    }

    #[test]
    fn printed_steps_name_the_docker_desktop_setting_and_its_reach() {
        let fake = Arc::new(Fake::healthy());
        up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap();
        let steps = fake.steps();
        assert!(steps.contains("Docker Desktop"), "{steps}");
        assert!(steps.contains("Settings > Advanced >"), "{steps}");
        assert!(
            steps.contains("Allow privileged port forwarding to external machines"),
            "{steps}"
        );
        assert!(steps.contains("below 1024"), "{steps}");
    }

    #[test]
    fn printed_steps_offer_the_tls_alpn_caa_record_and_what_leaving_it_out_costs() {
        let fake = Arc::new(Fake::healthy());
        up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap();
        let steps = fake.steps();
        assert!(steps.contains(&caa_record(DOMAIN)), "{steps}");
        assert!(steps.contains("Optional"), "{steps}");
        assert!(steps.contains("port 80"), "{steps}");
    }

    #[test]
    fn caa_record_allows_only_lets_encrypt_over_tls_alpn() {
        assert_eq!(
            caa_record(DOMAIN),
            r#"ingress.example.com. CAA 0 issue "letsencrypt.org; validationmethods=tls-alpn-01""#
        );
    }

    #[test]
    fn wait_for_enter_returns_at_once_when_stdin_is_not_a_terminal() {
        let mut input = Cursor::new(b"typed\n".to_vec());
        wait_for_enter(false, &mut input).unwrap();
        assert_eq!(input.position(), 0);
    }

    #[test]
    fn wait_for_enter_reads_one_line_when_stdin_is_a_terminal() {
        let mut input = Cursor::new(b"\nlater\n".to_vec());
        wait_for_enter(true, &mut input).unwrap();
        assert_eq!(input.position(), 1);
    }

    #[test]
    fn certificate_failure_or_timeout_prints_one_line_per_cause() {
        for (logs, deadline) in [
            (LOG_FAILED, Duration::from_secs(5)),
            (CERT_PENDING, Duration::ZERO),
        ] {
            let fake = Arc::new(Fake {
                logs,
                ..Fake::healthy()
            });
            let err = up(&fake, &UndoList::new(), deadline).unwrap_err();
            let lines: Vec<&str> = err.lines().collect();
            let mut at = Vec::new();
            for cause in CAUSES {
                let i = lines
                    .iter()
                    .position(|l| l.contains(cause))
                    .unwrap_or_else(|| panic!("no line for {cause:?}:\n{err}"));
                at.push(i);
            }
            let distinct: BTreeSet<usize> = at.iter().copied().collect();
            assert_eq!(distinct.len(), CAUSES.len(), "causes share a line:\n{err}");
            for cause in ["A record", "100.64.0.0/10", "double NAT"] {
                let line = lines.iter().find(|l| l.contains(cause)).unwrap();
                assert!(line.contains("WAN IP"), "no check on {line:?}");
            }
            let aaaa = lines.iter().find(|l| l.contains("(AAAA)")).unwrap();
            assert!(aaaa.contains("registrar"), "no check on {aaaa:?}");
        }
    }

    #[test]
    fn certificate_failure_quotes_the_lets_encrypt_error_line() {
        let fake = Arc::new(Fake {
            logs: LOG_FAILED,
            ..Fake::healthy()
        });
        let err = up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap_err();
        assert!(err.contains(LE_ERROR), "{err}");
    }

    #[test]
    fn k3d_port_failure_adds_the_rootless_docker_line_without_claiming_the_port_is_taken() {
        let fake = Arc::new(Fake {
            fail: Some("k3d_port_add"),
            ..Fake::healthy()
        });
        let err = up(&fake, &UndoList::new(), Duration::from_secs(5)).unwrap_err();
        assert!(err.contains("k3d could not publish host port 443"), "{err}");
        assert!(err.contains("rootless Docker"), "{err}");
        assert!(err.contains("net.ipv4.ip_unprivileged_port_start"), "{err}");
        assert!(!err.contains("taken"), "{err}");
        assert!(!err.contains("already forwarded"), "{err}");
    }

    #[test]
    fn certificate_failure_or_timeout_undoes_helm_namespace_and_k3d_in_that_order() {
        for (logs, deadline) in [
            (LOG_FAILED, Duration::from_secs(5)),
            (CERT_PENDING, Duration::ZERO),
        ] {
            let fake = Arc::new(Fake {
                logs,
                ..Fake::healthy()
            });
            assert!(up(&fake, &UndoList::new(), deadline).is_err(), "{logs}");
            assert_eq!(
                fake.changes(),
                [
                    "k3d_port_add",
                    "apply_namespace",
                    "helm_install",
                    "helm_uninstall",
                    "delete_namespace",
                    "k3d_port_delete",
                ],
                "{logs}"
            );
            if logs == LOG_FAILED {
                let reads = fake.calls().iter().filter(|c| **c == "caddy_logs").count();
                assert_eq!(reads, 1, "the first failure line must end the watch");
            }
        }
    }

    #[test]
    fn a_failed_change_undoes_exactly_the_changes_recorded_before_it() {
        let cases: [(&str, &[&str]); 4] = [
            ("wait_for_operator", &[]),
            ("k3d_port_add", &["k3d_port_add"]),
            (
                "apply_namespace",
                &[
                    "k3d_port_add",
                    "apply_namespace",
                    "delete_namespace",
                    "k3d_port_delete",
                ],
            ),
            (
                "helm_install",
                &[
                    "k3d_port_add",
                    "apply_namespace",
                    "helm_install",
                    "helm_uninstall",
                    "delete_namespace",
                    "k3d_port_delete",
                ],
            ),
        ];
        for (failing, want) in cases {
            let fake = Arc::new(Fake {
                fail: Some(failing),
                ..Fake::healthy()
            });
            assert!(
                up(&fake, &UndoList::new(), Duration::from_secs(5)).is_err(),
                "{failing}"
            );
            assert_eq!(fake.changes(), want, "{failing}");
        }
    }

    #[test]
    fn a_successful_run_leaves_the_undo_list_empty() {
        let fake = Arc::new(Fake::healthy());
        let undos = UndoList::new();
        up(&fake, &undos, Duration::from_secs(5)).unwrap();
        undos.drain();
        assert_eq!(
            fake.changes(),
            ["k3d_port_add", "apply_namespace", "helm_install"]
        );
    }

    #[test]
    fn ctrl_c_after_the_first_change_undoes_every_change_once_in_reverse() {
        let cases: [(&str, &[&str]); 3] = [
            ("wait_for_operator", &[]),
            (
                "apply_namespace",
                &[
                    "k3d_port_add",
                    "apply_namespace",
                    "delete_namespace",
                    "k3d_port_delete",
                ],
            ),
            (
                "helm_install",
                &[
                    "k3d_port_add",
                    "apply_namespace",
                    "helm_install",
                    "helm_uninstall",
                    "delete_namespace",
                    "k3d_port_delete",
                ],
            ),
        ];
        for (at, want) in cases {
            let (tx, rx) = channel();
            let fake = Arc::new(Fake {
                interrupt: Mutex::new(Some((at, tx))),
                ..Fake::healthy()
            });
            let undos = UndoList::new();
            let list = &undos;
            std::thread::scope(|s| {
                let handler = s.spawn(move || {
                    rx.recv_timeout(Duration::from_secs(5))
                        .expect("Ctrl-C never reached the handler");
                    list.drain();
                });
                assert!(up(&fake, list, Duration::from_secs(5)).is_err(), "{at}");
                handler.join().unwrap();
            });
            assert_eq!(fake.changes(), want, "{at}");
        }
    }

    const DOWN_CALLS: [&str; 7] = [
        "cluster_exists",
        "release_exists",
        "namespace_exists",
        "k3d_port_exists",
        "helm_uninstall",
        "delete_namespace",
        "k3d_port_delete",
    ];

    fn assert_down_calls_only_reads_and_removals(fake: &Fake) {
        let calls = fake.calls();
        assert!(
            calls.iter().all(|c| DOWN_CALLS.contains(c)),
            "down made a call outside reads and removals: {calls:?}"
        );
    }

    fn assert_operator_items(out: &str) {
        let lower = out.to_lowercase();
        let forward = out
            .lines()
            .find(|l| l.to_lowercase().contains("router"))
            .unwrap_or_else(|| panic!("no line names the router:\n{out}"));
        assert!(forward.to_lowercase().contains("forward"), "{forward}");
        assert!(forward.contains("443"), "{forward}");
        assert!(out.contains("Docker Desktop"), "{out}");
        assert!(
            out.contains("Allow privileged port forwarding to external machines"),
            "{out}"
        );
        assert!(lower.contains("nothing else"), "{out}");
        assert!(out.contains("A record"), "{out}");
        assert!(out.contains("CAA"), "{out}");
        assert!(lower.contains("new certificate"), "{out}");
    }

    #[test]
    fn down_removes_release_namespace_and_k3d_port_in_that_order_then_prints_operator_items() {
        let fake = Fake::with_live(&["release", "namespace", "k3d:443"]);
        let out = run_down(&fake).unwrap();
        assert_eq!(
            fake.changes(),
            ["helm_uninstall", "delete_namespace", "k3d_port_delete"]
        );
        assert_operator_items(&out);
        assert_down_calls_only_reads_and_removals(&fake);
    }

    #[test]
    fn down_removes_only_what_a_partial_up_left() {
        let cases: [(&[&str], &[&str]); 5] = [
            (&["namespace"], &["delete_namespace"]),
            (&["k3d:443"], &["k3d_port_delete"]),
            (&["release"], &["helm_uninstall"]),
            (
                &["namespace", "k3d:443"],
                &["delete_namespace", "k3d_port_delete"],
            ),
            (
                &["release", "namespace"],
                &["helm_uninstall", "delete_namespace"],
            ),
        ];
        for (left, want) in cases {
            let fake = Fake::with_live(left);
            let out = run_down(&fake).unwrap_or_else(|e| panic!("{left:?}: {e}"));
            assert_eq!(fake.changes(), want, "{left:?}");
            assert_operator_items(&out);
            assert_down_calls_only_reads_and_removals(&fake);
        }
    }

    #[test]
    fn down_with_no_ingress_changes_nothing_says_so_and_succeeds() {
        let fake = Fake::healthy();
        let out = run_down(&fake).unwrap();
        assert!(out.to_lowercase().contains("no ingress"), "{out}");
        assert_eq!(fake.changes(), Vec::<&str>::new());
        assert_down_calls_only_reads_and_removals(&fake);
        for read in ["release_exists", "namespace_exists", "k3d_port_exists"] {
            assert!(fake.calls().contains(&read), "{read} not checked");
        }
    }

    #[test]
    fn down_stops_at_a_failing_removal_and_names_the_step() {
        let cases: [(&str, &[&str], &str); 3] = [
            ("helm_uninstall", &["helm_uninstall"], "the release"),
            (
                "delete_namespace",
                &["helm_uninstall", "delete_namespace"],
                "the namespace",
            ),
            (
                "k3d_port_delete",
                &["helm_uninstall", "delete_namespace", "k3d_port_delete"],
                "port 443",
            ),
        ];
        for (failing, want, names) in cases {
            let fake = Fake {
                fail: Some(failing),
                ..Fake::with_live(&["release", "namespace", "k3d:443"])
            };
            let err = run_down(&fake).unwrap_err();
            assert_eq!(fake.changes(), want, "{failing}");
            assert!(err.contains(names), "{failing}: {err}");
            assert!(
                err.contains(&format!("{failing} broke")),
                "{failing}: {err}"
            );
        }
    }

    #[test]
    fn down_with_a_failing_presence_read_changes_nothing() {
        for read in ["release_exists", "namespace_exists", "k3d_port_exists"] {
            let fake = Fake {
                fail: Some(read),
                ..Fake::with_live(&["release", "namespace", "k3d:443"])
            };
            let err = run_down(&fake).expect_err(read);
            assert!(err.contains(&format!("{read} broke")), "{read}: {err}");
            assert_eq!(fake.changes(), Vec::<&str>::new(), "{read}");
        }
    }

    #[test]
    fn down_with_no_cluster_reads_only_the_cluster_changes_nothing_and_says_no_ingress() {
        let fake = Fake {
            cluster: false,
            ..Fake::with_live(&["release", "namespace", "k3d:443"])
        };
        let out = run_down(&fake).unwrap();
        assert!(out.to_lowercase().contains("no ingress"), "{out}");
        assert_eq!(fake.calls(), ["cluster_exists"]);
        assert_eq!(fake.changes(), Vec::<&str>::new());
    }

    // Error texts captured from live helm and kubectl runs.

    const HELM_NOT_FOUND: &str = "helm failed: Error: release: not found\n";
    const KUBECTL_NOT_FOUND: &str =
        "kubectl failed: Error from server (NotFound): namespaces \"sycophant-ingress\" not found\n";
    const HELM_UNREACHABLE: &str = "helm failed: Error: Kubernetes cluster unreachable: Get \"https://127.0.0.1:64079/version\": dial tcp 127.0.0.1:64079: connect: connection refused\n";
    const KUBECTL_UNREACHABLE: &str = "kubectl failed: The connection to the server 127.0.0.1:64079 was refused - did you specify the right host or port?\n";
    /// A "not found" that is not about the release or namespace: client-go's
    /// text when the kubeconfig has no such context.
    const KUBECTL_NO_CONTEXT: &str = "kubectl failed: Error in configuration: context was not found for specified context: k3d-sycophant\n";
    const NO_BINARY: &str = "failed to run helm: No such file or directory (os error 2)";

    #[test]
    fn presence_reads_a_successful_command_as_present() {
        assert_eq!(
            presence("the release", Ok("NAME: sycophant-ingress".to_string())),
            Ok(true)
        );
        assert_eq!(presence("the namespace", Ok(String::new())), Ok(true));
    }

    #[test]
    fn presence_reads_helm_and_kubectl_not_found_as_absent() {
        assert_eq!(
            presence("the release", Err(HELM_NOT_FOUND.to_string())),
            Ok(false)
        );
        assert_eq!(
            presence("the namespace", Err(KUBECTL_NOT_FOUND.to_string())),
            Ok(false)
        );
    }

    #[test]
    fn presence_stops_on_any_other_error_naming_what_could_not_be_read() {
        for (what, text) in [
            ("the release", HELM_UNREACHABLE),
            ("the namespace", KUBECTL_UNREACHABLE),
            ("the namespace", KUBECTL_NO_CONTEXT),
            ("the release", NO_BINARY),
        ] {
            let err = presence(what, Err(text.to_string()))
                .expect_err(&format!("{text:?} read as a real answer"));
            assert!(err.contains(what), "{what}: {err}");
            assert!(err.contains(text.trim()), "the cause is dropped: {err}");
        }
    }

    /// `docker port k3d-sycophant-serverlb` with ingress up.
    const PORTS_WITH_INGRESS: &str = "6443/tcp -> 0.0.0.0:64079\n\
        30443/tcp -> 127.0.0.1:30443\n\
        30444/tcp -> 0.0.0.0:443\n\
        30444/tcp -> [::]:443";
    const PORTS_WITHOUT_INGRESS: &str = "6443/tcp -> 0.0.0.0:64079\n\
        30443/tcp -> 127.0.0.1:30443";

    #[test]
    fn publishes_host_443_reads_the_live_docker_port_output() {
        assert!(publishes_host_443(PORTS_WITH_INGRESS));
        assert!(!publishes_host_443(PORTS_WITHOUT_INGRESS));
        assert!(!publishes_host_443(""));
    }

    #[test]
    fn publishes_host_443_never_matches_dex_30443_or_another_host_port() {
        for ports in [
            "30443/tcp -> 127.0.0.1:30443",
            "30443/tcp -> 0.0.0.0:443",
            "30444/tcp -> 0.0.0.0:8443",
            "30444/tcp -> 0.0.0.0:4443",
            "6443/tcp -> 0.0.0.0:443",
        ] {
            assert!(!publishes_host_443(ports), "{ports}");
        }
        assert!(publishes_host_443("30444/tcp -> [::]:443"));
    }

    #[test]
    fn serverlb_running_errs_unless_docker_says_true() {
        assert_eq!(serverlb_running("true"), Ok(()));
        assert_eq!(serverlb_running("true\n"), Ok(()));
        for state in ["false", "false\n", ""] {
            let err = serverlb_running(state).expect_err(state);
            assert!(err.contains("k3d-sycophant-serverlb"), "{state:?}: {err}");
            assert!(err.contains("not running"), "{state:?}: {err}");
        }
    }
}
