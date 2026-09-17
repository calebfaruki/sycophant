#!/usr/bin/env bash
# Offline acceptance suite for the OIDC identity-root substrate (framework half).
# Renders the sycophant-cluster chart with `helm template` only. No cluster.
#
#   bash tests/unit/oidc-identity-root/acceptance.sh
#
# Exits non-zero and prints every unmet expectation.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
CHART="$ROOT/charts/sycophant-cluster"
SCHEMA="$CHART/values.schema.json"
VALUES="$CHART/values.yaml"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
ok()   { pass=$((pass+1)); printf 'ok   %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf 'FAIL %s\n    %s\n' "$1" "$2"; }

render() { helm template c "$CHART" -n sycophant "$@" 2>&1; }

cat > "$WORK/valid.yaml" <<'YAML'
policyEngine: external
authEngine: oidc
idps:
  - issuer: https://idp-a.example.com
    audience: sycophant-cluster-aud
    groupsClaim:
      name: cluster-group-claim
YAML

cat > "$WORK/two.yaml" <<'YAML'
policyEngine: external
authEngine: oidc
idps:
  - issuer: https://idp-a.example.com
    audience: aud-a
    groupsClaim: { name: groups }
  - issuer: https://idp-b.example.com
    audience: aud-b
    groupsClaim: { name: roles }
YAML

cat > "$WORK/no-audience.yaml" <<'YAML'
policyEngine: external
authEngine: oidc
idps:
  - issuer: https://idp-a.example.com
    groupsClaim: { name: groups }
YAML

cat > "$WORK/no-groups.yaml" <<'YAML'
policyEngine: external
authEngine: oidc
idps:
  - issuer: https://idp-a.example.com
    audience: aud-a
YAML

cat > "$WORK/no-groups-name.yaml" <<'YAML'
policyEngine: external
authEngine: oidc
idps:
  - issuer: https://idp-a.example.com
    audience: aud-a
    groupsClaim: {}
YAML

cat > "$WORK/external.yaml" <<'YAML'
policyEngine: external
authEngine: external
YAML

cat > "$WORK/swapped.yaml" <<'YAML'
policyEngine: external
authEngine: oidc
idps:
  - issuer: https://idp-swapped.example.net
    audience: sycophant-cluster-aud
    groupsClaim:
      name: cluster-group-claim
YAML

cat > "$WORK/with-ca.yaml" <<'YAML'
policyEngine: external
authEngine: oidc
idps:
  - issuer: https://dex.internal.example
    audience: aud-ca
    groupsClaim: { name: groups }
    certificateAuthority: |
      -----BEGIN CERTIFICATE-----
      TESTCASENTINEL
      -----END CERTIFICATE-----
YAML

# authEngine required, no default: absent selector must abort the render.
t_authengine_required() {
  local out; out="$(render --set policyEngine=external)"; local rc=$?
  if [ $rc -eq 0 ]; then
    bad authengine_required "render succeeded with authEngine unset; it must be required with no default"
  elif ! grep -qi authEngine <<<"$out"; then
    bad authengine_required "render failed but not on authEngine: $(tail -1 <<<"$out")"
  else
    ok authengine_required
  fi
}

# Enum membership: oidc and external accepted, anything else rejected.
t_authengine_enum() {
  render --set policyEngine=external -f "$WORK/external.yaml" >/dev/null 2>&1 \
    && render -f "$WORK/valid.yaml" >/dev/null 2>&1
  local accept=$?
  render --set policyEngine=external --set authEngine=ldap >/dev/null 2>&1
  local reject=$?
  if [ $accept -ne 0 ]; then
    bad authengine_enum "a valid authEngine value (oidc/external) was rejected"
  elif [ $reject -eq 0 ]; then
    bad authengine_enum "authEngine=ldap rendered; the enum must reject out-of-range values"
  else
    ok authengine_enum
  fi
}

# external renders zero apiserver-trust resources.
t_external_no_trust() {
  local out; out="$(render -f "$WORK/external.yaml")"
  if [ $? -ne 0 ]; then
    bad external_no_trust "authEngine=external failed to render: $(tail -1 <<<"$out")"
  elif grep -q 'AuthenticationConfiguration' <<<"$out"; then
    bad external_no_trust "authEngine=external rendered an AuthenticationConfiguration; it must render none"
  else
    ok external_no_trust
  fi
}

# oidc renders StructuredAuthenticationConfiguration.
t_oidc_renders_config() {
  local out; out="$(render -f "$WORK/valid.yaml")"
  if [ $? -ne 0 ]; then
    bad oidc_renders_config "authEngine=oidc failed to render: $(tail -1 <<<"$out")"
  elif ! grep -q 'kind: AuthenticationConfiguration' <<<"$out"; then
    bad oidc_renders_config "no 'kind: AuthenticationConfiguration' in the oidc render"
  else
    ok oidc_renders_config
  fi
}

# oidc renders structured config, never the legacy apiserver flags.
t_oidc_no_legacy_flags() {
  local out; out="$(render -f "$WORK/valid.yaml")"
  if [ $? -ne 0 ]; then
    bad oidc_no_legacy_flags "oidc render failed: $(tail -1 <<<"$out")"
  elif grep -q -- '--oidc-' <<<"$out"; then
    bad oidc_no_legacy_flags "legacy --oidc-* flags present; the engine must render structured config only"
  else
    ok oidc_no_legacy_flags
  fi
}

# Each per-IdP block carries issuer, audience, groups claim; a missing groups
# claim is rejected. Identity is the stable subject, not an operator claim.
t_idp_carries_claims() {
  render -f "$WORK/no-groups.yaml" >/dev/null 2>&1 && {
    bad idp_carries_claims "a block without groupsClaim rendered; it must be required"; return; }
  local out; out="$(render -f "$WORK/valid.yaml")"
  if [ $? -ne 0 ]; then
    bad idp_carries_claims "valid oidc block failed to render: $(tail -1 <<<"$out")"
  elif ! grep -q 'claim: sub' <<<"$out"; then
    bad idp_carries_claims "identity is not mapped to the stable subject (sub)"
  elif ! grep -q 'cluster-group-claim' <<<"$out"; then
    bad idp_carries_claims "groupsClaim name not mapped into the rendered config"
  else
    ok idp_carries_claims
  fi
}

# groupsClaim.name is required: a groupsClaim without a name is rejected.
t_groups_name_required() {
  if render -f "$WORK/no-groups-name.yaml" >/dev/null 2>&1; then
    bad groups_name_required "a groupsClaim without name rendered; name must be required"
  else
    ok groups_name_required
  fi
}

# aud validation is mandatory: a block without an audience is rejected.
t_idp_requires_audience() {
  render -f "$WORK/valid.yaml" >/dev/null 2>&1 || {
    bad idp_requires_audience "a block with an audience was rejected"; return; }
  if render -f "$WORK/no-audience.yaml" >/dev/null 2>&1; then
    bad idp_requires_audience "a block without audience rendered; audience must be mandatory per-IdP"
  else
    ok idp_requires_audience
  fi
}

# Multiple per-IdP blocks are accepted and each issuer reaches the render.
t_multi_issuer() {
  local out; out="$(render -f "$WORK/two.yaml")"
  if [ $? -ne 0 ]; then
    bad multi_issuer "two per-IdP blocks failed to render: $(tail -1 <<<"$out")"
  elif ! grep -q 'https://idp-a.example.com' <<<"$out" || ! grep -q 'https://idp-b.example.com' <<<"$out"; then
    bad multi_issuer "both issuers must appear; multi-issuer render dropped one"
  else
    ok multi_issuer
  fi
}

# Each issuer namespaces identity with its own issuer prefix, so two issuers
# emitting the same sub cannot collapse to one RBAC identity.
t_issuer_prefixes_identity() {
  local out; out="$(render -f "$WORK/two.yaml")"
  if [ $? -ne 0 ]; then
    bad issuer_prefixes_identity "two per-IdP blocks failed to render: $(tail -1 <<<"$out")"
  elif ! grep -q 'prefix: "https://idp-a.example.com#"' <<<"$out" \
    || ! grep -q 'prefix: "https://idp-b.example.com#"' <<<"$out"; then
    bad issuer_prefixes_identity "each issuer must prefix identity with its own issuer; a shared or empty prefix lets subjects collide"
  else
    ok issuer_prefixes_identity
  fi
}

# The 1.30 floor is enforced structurally by Chart.yaml kubeVersion (Helm gates
# install on it); the 1.34 ungated floor is doc-only and lives in values.yaml prose.
t_floor_documented() {
  if ! grep -qE 'kubeVersion:.*1\.30' "$CHART/Chart.yaml"; then
    bad floor_documented "Chart.yaml kubeVersion must enforce the 1.30 floor"
  elif ! grep -q '1\.34' "$VALUES" || ! grep -qi 'ungated' "$VALUES"; then
    bad floor_documented "values.yaml must document the 1.34 ungated floor"
  else
    ok floor_documented
  fi
}

# No IdP signing key material rides in the rendered apiserver-trust config.
t_no_signing_key() {
  local out; out="$(render -f "$WORK/valid.yaml")"
  if [ $? -ne 0 ]; then
    bad no_signing_key "oidc render failed: $(tail -1 <<<"$out")"
  elif ! grep -q 'kind: AuthenticationConfiguration' <<<"$out"; then
    bad no_signing_key "no auth config rendered to inspect for key material"
  elif grep -qiE 'BEGIN [A-Z ]*PRIVATE KEY|jwks|signingkey|signing-key|signing_key' <<<"$out"; then
    bad no_signing_key "the rendered config embeds signing-key or JWKS material; the key is the IdP's"
  else
    ok no_signing_key
  fi
}

# The hobbyist bundled issuer is one oidc issuer, not a third authEngine value.
t_no_third_engine() {
  local enum; enum="$(jq -c '(.properties.authEngine.enum // []) | sort' "$SCHEMA" 2>/dev/null)"
  if [ "$enum" != '["external","oidc"]' ]; then
    bad no_third_engine "authEngine enum must be exactly {oidc, external}; got: $enum"
    return
  fi
  local out; out="$(render -f "$WORK/valid.yaml")"
  if [ $? -ne 0 ] || ! grep -q 'kind: AuthenticationConfiguration' <<<"$out"; then
    bad no_third_engine "a bundled-style in-cluster issuer must render as one oidc idps entry"
  else
    ok no_third_engine
  fi
}

# Swapping the issuer is a config change: the rendered issuer URL tracks input,
# and no per-issuer key material is carried.
t_issuer_swap_config_only() {
  local a b; a="$(render -f "$WORK/valid.yaml")"; b="$(render -f "$WORK/swapped.yaml")"
  if ! grep -q 'https://idp-a.example.com' <<<"$a"; then
    bad issuer_swap_config_only "first issuer URL absent from its render"
  elif ! grep -q 'https://idp-swapped.example.net' <<<"$b"; then
    bad issuer_swap_config_only "swapped issuer URL absent; swap is not config-driven"
  elif grep -q 'https://idp-a.example.com' <<<"$b"; then
    bad issuer_swap_config_only "old issuer survived a config swap; issuer is hardcoded somewhere"
  else
    ok issuer_swap_config_only
  fi
}

# A self-hosted issuer's TLS CA is a config value: present in a block, it rides
# into the rendered config; absent, no certificateAuthority is emitted (public
# issuer uses the system trust store). Without this the bundled/self-hosted
# issuer is untrustable without an apiserver trust-store edit.
t_ca_config_optional() {
  local with; with="$(render -f "$WORK/with-ca.yaml")"
  local without; without="$(render -f "$WORK/valid.yaml")"
  if [ $? -ne 0 ] || ! grep -q 'kind: AuthenticationConfiguration' <<<"$with"; then
    bad ca_config_optional "a block carrying certificateAuthority failed to render: $(tail -1 <<<"$with")"
  elif ! grep -q 'certificateAuthority:' <<<"$with" || ! grep -q 'TESTCASENTINEL' <<<"$with"; then
    bad ca_config_optional "certificateAuthority not carried into the render; a self-hosted issuer stays untrustable"
  elif grep -q 'certificateAuthority' <<<"$without"; then
    bad ca_config_optional "certificateAuthority emitted for a block that set none; it must be optional"
  else
    ok ca_config_optional
  fi
}

t_authengine_required
t_authengine_enum
t_external_no_trust
t_oidc_renders_config
t_oidc_no_legacy_flags
t_idp_carries_claims
t_groups_name_required
t_idp_requires_audience
t_multi_issuer
t_issuer_prefixes_identity
t_floor_documented
t_no_signing_key
t_no_third_engine
t_issuer_swap_config_only
t_ca_config_optional

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
