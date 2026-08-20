//! Grant-row parsing for the `grants` ConfigMap — the relay's routing and
//! authorization table.
//!
//! One ConfigMap key per grant row; the value is the row's three fields.
//! Validation is invalid-is-absent: a row that does not parse does not
//! exist, and it never suppresses the rows beside it. There is no default,
//! no last-known-good, and no blue-green swap over the whole map, because a
//! typo in one row must not block a revocation made in the same edit.
//!
//! Pure — no I/O. `grants_watcher` owns the delivery side.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::ConfigMap;
use serde::Deserialize;

/// The ConfigMap the relay watches. One per tenant namespace.
pub const GRANTS_CONFIGMAP_NAME: &str = "grants";

/// Channels a row may name. A row naming anything else is absent.
pub const KNOWN_CHANNELS: &[&str] = &["app", "email", "telegram"];

/// Channels whose identity is an operator-invented code rather than a
/// platform handle. Only these rows are redeemable on the app port.
pub const OPERATOR_VERIFIED_CHANNELS: &[&str] = &["app"];

/// One authorization row: `(channel, identity, workspace)`.
///
/// No scope, capability, or profile field. One conversational surface for
/// every row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantRow {
    pub channel: String,
    pub identity: String,
    pub workspace: String,
}

/// A row that failed validation, named by its ConfigMap key so the operator
/// can find it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowError {
    pub key: String,
    pub reason: String,
}

/// The live authorization table. Replaced wholesale on every delivery.
#[derive(Clone, Debug, Default)]
pub struct GrantsTable {
    rows: BTreeMap<String, GrantRow>,
}

impl GrantsTable {
    pub fn get(&self, row_key: &str) -> Option<&GrantRow> {
        self.rows.get(row_key)
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The operator-verified row whose identity is `code`, if any. The
    /// identity IS the code; the row key is operator-chosen prose and is
    /// never matched against.
    pub fn find_by_code(&self, code: &str) -> Option<(&str, &GrantRow)> {
        self.rows.iter().find_map(|(key, row)| {
            (OPERATOR_VERIFIED_CHANNELS.contains(&row.channel.as_str()) && row.identity == code)
                .then_some((key.as_str(), row))
        })
    }
}

impl FromIterator<(String, GrantRow)> for GrantsTable {
    fn from_iter<I: IntoIterator<Item = (String, GrantRow)>>(iter: I) -> Self {
        Self {
            rows: iter.into_iter().collect(),
        }
    }
}

/// The row's on-the-wire shape. `deny_unknown_fields` is what keeps a
/// scope, capability, or profile field out: a row carrying one is a row the
/// relay does not understand, so it is absent.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RowFields {
    channel: String,
    identity: String,
    workspace: String,
}

/// Parse and validate one row's text, independently of every other row.
pub fn parse_row(key: &str, yaml: &str) -> Result<GrantRow, RowError> {
    let reject = |reason: String| RowError {
        key: key.to_string(),
        reason,
    };

    let fields: RowFields =
        serde_yaml::from_str(yaml).map_err(|e| reject(format!("row does not parse: {e}")))?;

    if !KNOWN_CHANNELS.contains(&fields.channel.as_str()) {
        return Err(reject(format!("unknown channel: {}", fields.channel)));
    }
    if fields.identity.trim().is_empty() {
        return Err(reject("identity is empty".into()));
    }
    if fields.workspace.trim().is_empty() {
        return Err(reject("workspace is empty".into()));
    }

    Ok(GrantRow {
        channel: fields.channel,
        identity: fields.identity,
        workspace: fields.workspace,
    })
}

/// Build the table one delivery carries, plus the errors it raised. The
/// returned table is the whole truth: a row absent here is revoked, whether
/// the operator deleted it or broke it.
pub fn apply_delivery(cm: &ConfigMap) -> (GrantsTable, Vec<RowError>) {
    let mut rows = BTreeMap::new();
    let mut errors = Vec::new();

    for (key, body) in cm.data.iter().flatten() {
        match parse_row(key, body) {
            Ok(row) => {
                rows.insert(key.clone(), row);
            }
            Err(e) => errors.push(e),
        }
    }

    (GrantsTable { rows }, errors)
}
