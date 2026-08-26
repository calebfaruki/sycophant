# prompt-profiles

What a prompt profile may declare (values schema and rendered ConfigMap).

- `secret-optional` — a profile omitting `secret` passes the values schema
  and survives into the rendered profiles ConfigMap; the six-key bound,
  unknown-key reject, and missing-required-key reject all stay enforced.
