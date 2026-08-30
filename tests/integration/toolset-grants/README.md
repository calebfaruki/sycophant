# toolset-grants

The values schema is the first gate on toolset and grant shape, catching operator
error at `helm upgrade` time before any pod is scheduled.

- `entry-credential-and-egress-keys-rejected` — a toolset entry carries runtime
  shape only; a `secrets` or `egress` key fails the render by name, so a
  credential or egress hole can never be silently dropped.
- `grant-schema-bounds` — malformed grants (missing or empty `secret`, a
  relative or empty `path`, a non-RFC-1123 name, unknown keys, an empty menu, an
  unnamed entry) fail the render; both the bare-string and grant-bearing entry
  forms render as YAML.
