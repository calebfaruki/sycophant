# Self-host headscale with real ACME (HTTPS)

For self-host operators who want a phone (or any external Tailscale client) to connect to a sycophant deployment running on their own machine. Headscale gets a real Let's Encrypt cert via the bundled chart's ACME support; clients reach it over the public internet via a domain the operator controls.

This is the operator-side dance Phase 2 was designed to support. It's also the most operationally heavy of the deployment shapes. **For DevOps deployments** (cluster on cloud K8s with proper ingress + cert-manager), most of this isn't needed — you just point an Ingress with a managed cert at the headscale Service and you're done. **For SaaS deployments**, the tsnet bridge + headscale stack is bypassed entirely (customers get a subdomain on `*.sycophant.md` with a wildcard cert).

## Prerequisites

- A domain you control with DNS managed somewhere you can edit (Cloudflare, Route 53, etc.)
- Public IPv4 address on the operator's machine (i.e. NOT behind CGNAT)
- Home router admin access for port-forwarding
- Residential ISP that doesn't block inbound :80 / :443 (Orange France works; some US ISPs block these on residential)
- macOS with `kubectl`, `helm`, `k3d`, `flutter` (for Flutter testing) — see [`e2e-test.md`](e2e-test.md) for the full e2e-side prereqs
- Sycophant cluster + chart deployed per the e2e bootstrap (Steps 0-3 of `e2e-test.md`)

## DNS setup

Add an `A` record for the headscale subdomain:

| Type | Name           | Value (your public IPv4) | Proxy status         |
|------|----------------|--------------------------|----------------------|
| A    | `hs.<domain>`  | e.g. `86.238.13.112`     | **DNS-only** (off)   |

**Cloudflare proxying must be disabled.** The orange-cloud "Proxied" mode terminates TLS at Cloudflare's edge and re-issues to your origin, which interferes with both:
- The Let's Encrypt HTTP-01 challenge (Cloudflare may serve its own response on `/.well-known/acme-challenge/...` instead of forwarding)
- The Tailscale TS2021 noise-protocol upgrade (same WebSocket-passthrough issue documented in "What doesn't work" below)

Set the record to gray-cloud "DNS only."

Verify propagation: `dig +short hs.<domain> A` returns your public IP.

## Router port-forward

Configure your router to forward inbound :80 and :443 to your Mac's LAN IP. UI varies by router; on an Orange Livebox the path is **Réseau** → **Configuration NAT/PAT** → add two TCP rules:

| Application name | Protocol | External port | Internal IP    | Internal port |
|------------------|----------|---------------|----------------|---------------|
| `headscalehttp`  | TCP      | 80            | `192.168.1.x`  | 80            |
| `headscalehttps` | TCP      | 443           | `192.168.1.x`  | 443           |

Find your Mac's LAN IP with `ipconfig getifaddr en0` (or `en1` for secondary interface).

ISP sanity check: from a phone on cellular data (NOT your home WiFi), browse to `http://hs.<domain>`. You should get *something* — even a "connection refused" / "no service listening" response from your Mac means the router forward is reaching it. Total timeout means the router isn't forwarding or the ISP is blocking.

## Helm upgrade with ACME enabled

```sh
helm upgrade --install <release> charts/sycophant-tenant/ \
  -n <namespace> \
  -f <your values files> \
  --set headscale.enabled=true \
  --set headscale.serverUrl=https://hs.<domain> \
  --set headscale.acme.enabled=true \
  --set headscale.acme.email=you@<domain> \
  --set tsnetBridge.enabled=true \
  --set tsnetBridge.loginServer=https://hs.<domain>
```

The chart will:
- Configure headscale's `tls_letsencrypt_*` keys
- Switch the headscale Service to expose ports `:443` (HTTPS API) and `:80` (HTTP-01 challenge), in addition to `:9090` (metrics)
- Add `CAP_NET_BIND_SERVICE` to the headscale container so non-root can bind privileged ports
- Re-roll headscale and the tsnet bridge to use the new URL

## Bridge to your Mac (sudo port-forward)

The cluster's headscale Service is `ClusterIP`, only reachable from inside the cluster. To make it reachable from the public internet via your router-forwarded `:80` + `:443`, port-forward the Service to all-interfaces on your Mac. **In a terminal, kept open for the duration of testing:**

```sh
sudo kubectl port-forward --address 0.0.0.0 \
  -n <namespace> svc/headscale 80:80 443:443
```

`sudo` is required because :80 and :443 are privileged ports. `--address 0.0.0.0` is required so the router-incoming traffic on your en0 interface reaches it (default `127.0.0.1` only listens on loopback).

## Trigger ACME issuance

Make a request to the public URL:

```sh
curl -v https://hs.<domain>/health
```

The first request triggers headscale's autocert flow: it talks to Let's Encrypt, requests a challenge, serves it on `:80`, LE verifies, cert issues, headscale serves the API over HTTPS. Should return `{"status":"pass"}` within ~30 seconds. Cert + state persist in `/var/lib/headscale/cache` inside the headscale PVC, so renewal-on-restart works.

Confirm in headscale logs:

```sh
kubectl logs -n <namespace> deploy/headscale | grep -E '(obtained|certificate)'
```

## Sign a Tailscale client into your headscale

Mint a pre-auth key for the client (any external device — your Mac, a phone running the official Tailscale Android app, etc.):

```sh
kubectl exec -n <namespace> deploy/headscale -- headscale users create caleb
USER_ID=$(kubectl exec -n <namespace> deploy/headscale -- \
  headscale users list -o json | jq '.[] | select(.name=="caleb") | .id')
kubectl exec -n <namespace> deploy/headscale -- \
  headscale preauthkeys create -u "$USER_ID" -e 24h | tail -1
```

(headscale 0.28+ requires a numeric user ID for `-u`, not a username — hence the lookup.)

On the Mac:

```sh
sudo tailscale logout                   # leaves any existing tailnet (e.g. your hosted Tailscale)
sudo tailscale up --login-server=https://hs.<domain> \
                  --auth-key=<the key from above>
tailscale status                        # should list `tightbeam` (the bridge) as a peer
tailscale ping tightbeam                # should pong via DERP <region> in N ms
```

Or via the Tailscale Mac GUI: profile menu → "Use an alternate server" → `https://hs.<domain>` → enter the auth key.

For a phone: install the official Tailscale Android app, **Settings** → kebab menu (⋮) → **Use an alternate server** → enter `https://hs.<domain>`, log in via auth-key.

## Test from the Flutter app (or any direct gRPC client)

The Flutter app (sycophant's e2e-testing client) inherits the host's network when run on the emulator, so once the Mac is on your headscale tailnet, `tightbeam.ts.local:9090` (the bridge's MagicDNS hostname) resolves and routes through the tailnet. Authorize the device with an Enrollment CR (`syco tenant enrollment set <name> --ns <namespace> --workspace <ws>`), then read the one-time enrollment code the controller minted:

```sh
kubectl get enr -n <namespace> <name> \
  -o jsonpath='{.status.enrollmentCode}'
```

Paste the resulting code into the Flutter app's enrollment screen (along with the workspace name). See [`flutter-app.md`](flutter-app.md) for build/sideload of the app itself.

## Gotchas

- **Port-forward dies on every helm rollout.** Whenever `helm upgrade` rolls the headscale pod (config changes, cert refresh, etc.), the existing port-forward terminates with a stale-pod error. Restart the `sudo kubectl port-forward` command after any helm operation that touches headscale.
- **Bridge takes ~10s to register after pod roll.** After helm upgrade, the `tightbeam-tsnet-bridge` pod restarts and re-registers with headscale. `headscale nodes list` will show no bridge for ~10 seconds; logs will show the registration handshake. Wait it out.
- **Tailscale Mac CLI logout drops your existing tailnet.** `sudo tailscale logout` followed by `sudo tailscale up --login-server=...` switches your Mac off whatever Tailscale account it was on. If you have a personal hosted Tailscale tailnet you use for other things, log back into it when done testing here. The Tailscale Mac GUI may support multiple accounts simultaneously; the CLI's account model is single-tenant.
- **Cloudflare DNS proxy must stay off.** If you (or someone else managing the zone) flips on the orange-cloud Proxied mode after initial setup, the next ACME renewal will fail. The cert will expire after 90 days. Periodic verification: `curl -v https://hs.<domain>/health` should report a Let's Encrypt-issued cert, not a Cloudflare-managed one.
- **First ACME issuance can take a minute.** If you `curl https://hs.<domain>/health` immediately after the helm upgrade, you may get a TLS handshake failure for ~30s while autocert negotiates the challenge. Wait and retry.
- **headscale 0.28+ needs numeric user ID for preauthkeys.** Don't pass a username to `headscale preauthkeys create -u …`. Look up the ID from `headscale users list -o json` first.

## What doesn't work (and why)

Documenting these so the next person doesn't waste time on dead ends.

### Cloudflare tunnels (cloudflared) — both quick AND named — fail with headscale's TS2021 protocol

Tried path:
- `cloudflared tunnel --url http://localhost:8080` (quick tunnel) — gives a `*.trycloudflare.com` URL
- `cloudflared tunnel login && cloudflared tunnel create … && cloudflared tunnel route dns …` (named tunnel with config file) — gives a real subdomain on a Cloudflare-managed zone

Both got HTTP requests through (curl `https://<tunnel-url>/health` returned `{"status":"pass"}`), but Tailscale clients connecting to headscale via the tunnel failed with:

```
Received error: register request: Post "https://<tunnel-url>/machine/register":
  unexpected HTTP response: 500 Internal Server Error
```

And in headscale logs:

```
WRN noise.go:62 > No Upgrade header in TS2021 request.
   If headscale is behind a reverse proxy, make sure it is configured to pass
   WebSockets through.
```

Tailscale's TS2021 protocol ("noise") relies on an HTTP `Upgrade` header to switch from HTTPS to a long-lived noise-encrypted stream. Cloudflare tunnels (both quick and named) appear to strip or fail to propagate this header through the HTTP/2 → HTTP/1.1 conversion at the tunnel edge. Multiple headscale issue-tracker reports confirm this is a known incompatibility, not a config problem.

**Workaround:** none. Use direct HTTPS (the path documented above) or a different tunnel implementation that doesn't have this issue (untested — possibly ngrok with a paid account, or a self-hosted reverse proxy like Caddy).

### Plain HTTP localhost as a Tailscale Mac control plane URL

Tried path: skip the domain + cert dance entirely by running headscale on the Mac with `server_url: http://localhost:8080`, then sign Mac Tailscale into it via "Use alternate server: http://localhost:8080".

Result: Tailscale Mac rejects non-HTTPS control plane URLs. There's no documented or undocumented flag to relax this.

**Workaround:** none for the Mac client. The Tailscale Android client has historically been more permissive but should not be relied on.

### Same-URL-from-cluster-and-host headscale config

Tried mental path: run headscale on the Mac (not in the cluster), have the in-cluster bridge connect to it via `host.k3d.internal:8080` while the Mac connects to it via `localhost:8080`. Two clients, same headscale, two URLs.

Result: doesn't work because headscale's `server_url` config field is a single URL that gets stamped into auth keys and used by all clients. If `server_url` is `http://localhost:8080`, the in-cluster bridge can't use that URL. If it's `http://host.k3d.internal:8080`, the Mac can't.

**Workaround:** put headscale somewhere both endpoints can reach via the same URL — which loops back to the public-domain + ACME path documented at the top of this file, OR putting headscale on the Mac's LAN IP (`http://192.168.1.x:8080`) which works on the local network but still hits the HTTPS-required-by-Tailscale-Mac wall.
