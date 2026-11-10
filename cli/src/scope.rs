use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

/// The sycoadm config root (`~/.config/sycophant`, or `SYCO_CONFIG_DIR` when set) —
/// charts, instructions, tenants, Dex trust, and crash reports all live under
/// it. `None` when neither is available; `Scope::global` surfaces that as an
/// error, while the panic-time crash reporter falls back to the temp dir. Single
/// source of truth for the root path.
pub(crate) fn config_dir() -> Option<PathBuf> {
    config_dir_from(env::var_os("SYCO_CONFIG_DIR"), env::var_os("HOME"))
}

fn config_dir_from(over: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    match over {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home.map(|h| PathBuf::from(h).join(".config").join("sycophant")),
    }
}

/// A sycophant config scope rooted at the global config dir
/// (`~/.config/sycophant`). Charts/examples live at the root; when `tenant` is
/// set, `release_name` resolves to that tenant.
pub(crate) struct Scope {
    pub root: PathBuf,
    pub tenant: Option<String>,
}

impl Scope {
    /// Global scope (no tenant) — `sycoadm setup` scaffolds charts/examples here.
    pub(crate) fn global() -> Result<Self, String> {
        Ok(Scope {
            root: config_dir()
                .ok_or_else(|| "neither SYCO_CONFIG_DIR nor HOME is set".to_string())?,
            tenant: None,
        })
    }

    /// Global scope bound to a named tenant: `release_name()` returns the
    /// tenant, chart dirs stay global.
    pub(crate) fn for_tenant(ns: &str) -> Result<Self, String> {
        let mut s = Self::global()?;
        s.tenant = Some(ns.to_string());
        Ok(s)
    }

    pub(crate) fn chart_dir(&self, name: &str) -> PathBuf {
        self.root.join("charts").join(name)
    }
    pub(crate) fn commands_chart_dir(&self) -> PathBuf {
        self.root.join("charts").join("sycophant-commands")
    }
    pub(crate) fn cluster_chart_dir(&self) -> PathBuf {
        self.root.join("charts").join("sycophant-cluster")
    }
    pub(crate) fn gvisor_chart_dir(&self) -> PathBuf {
        self.root.join("charts").join("sycophant-gvisor")
    }
    pub(crate) fn dex_chart_dir(&self) -> PathBuf {
        self.root.join("charts").join("sycophant-dex")
    }
    pub(crate) fn kyverno_crds_chart_dir(&self) -> PathBuf {
        self.root.join("charts").join("kyverno-crds")
    }
    /// Local-instructions content root (`~/.config/sycophant/instructions`). `setup`
    /// bind-mounts this into the k3d node so HostPath-instructions PVs resolve.
    pub(crate) fn instructions_dir(&self) -> PathBuf {
        self.root.join("instructions")
    }
    /// Dex trust: the apiserver auth config, Dex's public CA cert, and the
    /// operator's login email.
    pub(crate) fn oidc_dir(&self) -> PathBuf {
        self.root.join("oidc")
    }
    pub(crate) fn version_file(&self) -> PathBuf {
        self.root.join("version")
    }

    /// The tenant (namespace) this scope is bound to.
    pub(crate) fn release_name(&self) -> Result<String, String> {
        self.tenant
            .clone()
            .ok_or_else(|| "no tenant in scope (cluster-level command?)".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn global() -> Scope {
        Scope {
            root: PathBuf::from("/home/user/.config/sycophant"),
            tenant: None,
        }
    }

    #[test]
    fn config_dir_override_wins_over_home() {
        assert_eq!(
            config_dir_from(Some("/tmp/e2e".into()), Some("/home/u".into())),
            Some(PathBuf::from("/tmp/e2e"))
        );
        assert_eq!(
            config_dir_from(Some("".into()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.config/sycophant"))
        );
        assert_eq!(config_dir_from(None, None), None);
    }

    #[test]
    fn release_name_is_the_tenant_and_errors_without_one() {
        let s = Scope {
            root: PathBuf::from("/r"),
            tenant: Some("demo".into()),
        };
        assert_eq!(s.release_name().unwrap(), "demo");
        assert!(global().release_name().is_err());
    }
}
