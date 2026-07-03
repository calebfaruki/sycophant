use std::env;
use std::path::PathBuf;

/// A sycophant config scope rooted at the global config dir
/// (`~/.config/sycophant`). Charts/examples live at the root; when `tenant` is
/// set (every `syco tenant … --ns <name>` command), the namespace-scoped
/// accessors (`release_name`, `values_file`) resolve to that tenant.
pub(crate) struct Scope {
    pub root: PathBuf,
    pub tenant: Option<String>,
}

impl Scope {
    /// Global scope (no tenant) — `syco setup` scaffolds charts/examples here.
    pub(crate) fn global() -> Result<Self, String> {
        let home = env::var("HOME").map_err(|_| "HOME not set".to_string())?;
        Ok(Scope {
            root: PathBuf::from(home).join(".config").join("sycophant"),
            tenant: None,
        })
    }

    /// Global scope bound to a named tenant: `release_name()` returns the
    /// tenant, `values_file()` resolves the per-tenant values, chart dirs stay
    /// global.
    pub(crate) fn for_tenant(ns: &str) -> Result<Self, String> {
        let mut s = Self::global()?;
        s.tenant = Some(ns.to_string());
        Ok(s)
    }

    pub(crate) fn tenant_chart_dir(&self) -> PathBuf {
        self.root.join("charts").join("sycophant-tenant")
    }
    pub(crate) fn cluster_chart_dir(&self) -> PathBuf {
        self.root.join("charts").join("sycophant-cluster")
    }
    pub(crate) fn gvisor_chart_dir(&self) -> PathBuf {
        self.root.join("charts").join("sycophant-gvisor")
    }
    pub(crate) fn kyverno_crds_chart_dir(&self) -> PathBuf {
        self.root.join("charts").join("kyverno-crds")
    }
    /// Local-kernel content root (`~/.config/sycophant/kernels`). `setup`
    /// bind-mounts this into the k3d node so HostPath-kernel PVs resolve.
    pub(crate) fn kernels_dir(&self) -> PathBuf {
        self.root.join("kernels")
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

    /// Per-tenant values: `~/.config/sycophant/tenants/<ns>/values.yaml`.
    pub(crate) fn values_file(&self) -> PathBuf {
        match &self.tenant {
            Some(t) => self.root.join("tenants").join(t).join("values.yaml"),
            None => self.root.join("values.yaml"),
        }
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
    fn chart_dirs_are_global() {
        let s = global();
        assert_eq!(
            s.tenant_chart_dir(),
            PathBuf::from("/home/user/.config/sycophant/charts/sycophant-tenant")
        );
        assert_eq!(
            s.cluster_chart_dir(),
            PathBuf::from("/home/user/.config/sycophant/charts/sycophant-cluster")
        );
        assert_eq!(
            s.gvisor_chart_dir(),
            PathBuf::from("/home/user/.config/sycophant/charts/sycophant-gvisor")
        );
        assert_eq!(
            s.kyverno_crds_chart_dir(),
            PathBuf::from("/home/user/.config/sycophant/charts/kyverno-crds")
        );
    }

    #[test]
    fn tenant_values_file_is_per_tenant() {
        // Mutant dropping the tenant branch (→ root/values.yaml) is caught here.
        let s = Scope {
            root: PathBuf::from("/r"),
            tenant: Some("foo".into()),
        };
        assert_eq!(s.values_file(), PathBuf::from("/r/tenants/foo/values.yaml"));
    }

    #[test]
    fn global_values_file_is_root() {
        assert_eq!(
            global().values_file(),
            PathBuf::from("/home/user/.config/sycophant/values.yaml")
        );
    }

    #[test]
    fn release_name_is_the_tenant() {
        // Mutant returning a constant / ignoring tenant is caught here.
        let s = Scope {
            root: PathBuf::from("/r"),
            tenant: Some("demo".into()),
        };
        assert_eq!(s.release_name().unwrap(), "demo");
    }

    #[test]
    fn release_name_errors_without_tenant() {
        assert!(global().release_name().is_err());
    }
}
