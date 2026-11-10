#!/usr/bin/env bash
# Offline acceptance suite for the apiserver auth-config render.
# Renders the sycophant-cluster chart with `helm template` only. No cluster.
#
#   bash tests/unit/oidc-identity-root/acceptance.sh
#
# Covers the per-identity trust model:
# the chart always renders AuthenticationConfiguration off the `idp` contract,
# maps identity as the issuer-prefixed `sub` only, and guards the reserved
# `system:` prefix on the raw claim. Exits non-zero and prints every unmet
# expectation.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE
source "$HERE/../lib.sh"
CHART="$ROOT/charts/sycophant-cluster"
readonly CHART

render() {
  helm template c "$CHART" -n sycophant \
    --api-versions policies.kyverno.io/v1/ValidatingPolicy \
    --show-only templates/apiserver-auth-config.yaml "$@" 2>&1
}

# The whole trust config the per-identity model needs: issuer + audience, no
# groups. Identity is the prefixed sub; no group claim is mapped or consulted.
cat >"$WORK/one.yaml" <<'YAML'
idp:
  issuer: https://idp-a.example.com
  audience: aud-a
YAML

# The auth config's whole trust shape, from one render of a single issuer:
# - the issuer carries its audience and an issuer-derived username prefix
#   (<issuer>#), so the subject namespace is the issuer's own;
# - claim mapping is username/sub only: no groups, uid or extra mapping;
# - one claimValidationRules CEL rule rejects the reserved system: prefix on
#   the RAW sub, and there is no userValidationRules block (a post-prefix rule
#   would never fire);
# - the apiserver answers the service-account key set anonymously, and nothing
#   else: anonymous access is enabled with exactly one condition, path
#   /openid/v1/jwks;
# - `sycoadm setup` appends extra issuers to the end of the rendered file, so
#   `jwt:` must stay the last top-level key.
t_auth_config_shape() {
  local out
  if ! out="$(render -f "$WORK/one.yaml")"; then
    bad auth_config_shape \
      "single-issuer config failed to render: $(helm_err <<<"$out")"
    return
  fi
  printf '%s\n' "$out" >"$WORK/auth.yaml"
  local verdict
  verdict="$(
    python3 - "$WORK/auth.yaml" 2>&1 <<'PY'
import sys, yaml
cm = yaml.safe_load(open(sys.argv[1]))
text = (cm.get("data") or {}).get("auth-config.yaml") or ""
doc = yaml.safe_load(text) or {}
jwt = doc.get("jwt") or [{}]
issuer = jwt[0]
keys = [l.split(":")[0] for l in text.splitlines() if l and not l[0].isspace()
        and ":" in l]
checks = [
    ("kind", doc.get("kind"), "AuthenticationConfiguration"),
    ("jwt[0].issuer", issuer.get("issuer"),
      {"url": "https://idp-a.example.com", "audiences": ["aud-a"]}),
    ("jwt[0].claimMappings", issuer.get("claimMappings"),
      {"username": {"claim": "sub", "prefix": "https://idp-a.example.com#"}}),
    ("jwt[0].claimValidationRules expressions",
      [r.get("expression") for r in issuer.get("claimValidationRules") or []],
      ['!claims.sub.startsWith("system:")']),
    ("jwt[0].userValidationRules", "userValidationRules" in issuer, False),
    ("anonymous", doc.get("anonymous"),
      {"enabled": True, "conditions": [{"path": "/openid/v1/jwks"}]}),
    ("last top-level key", keys[-1:], ["jwt"]),
]
wrong = [f"{name}: want {want!r}, got {got!r}" for name, got, want in checks
          if got != want]
print("; ".join(wrong) if wrong else "OK")
PY
  )"
  if [ "$verdict" = "OK" ]; then
    ok auth_config_shape
  else
    bad auth_config_shape "$verdict"
  fi
}

t_auth_config_shape

summary
