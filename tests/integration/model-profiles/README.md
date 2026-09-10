# model-profiles

What a model entry may declare (values schema and rendered ConfigMap).

- `secret-optional` — an entry omitting `secret` passes the values schema and
  survives into the rendered model config; the five-key bound, unknown-key
  reject, and missing-required-key reject all stay enforced.
- `model-key-bounds` — an entry carries exactly `image`, `format`, `model`,
  `baseUrl`, and `secret`; `egress` and any other key are refused by name at the
  values schema, so an entry states its destination once. The root object is
  closed and no longer declares `prompt`, so a leftover turn-server block is
  refused rather than ignored.
