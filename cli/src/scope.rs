use std::env;
use std::fs;
use std::path::PathBuf;

pub(crate) struct Scope {
    pub root: PathBuf,
}

impl Scope {
    pub(crate) fn charts_dir(&self) -> PathBuf {
        self.root.join("charts").join("sycophant")
    }

    pub(crate) fn examples_dir(&self) -> PathBuf {
        self.root.join("examples")
    }

    pub(crate) fn version_file(&self) -> PathBuf {
        self.root.join("version")
    }

    pub(crate) fn release_file(&self) -> PathBuf {
        self.root.join("release")
    }

    pub(crate) fn release_name(&self) -> Result<String, String> {
        let path = self.release_file();
        let name = fs::read_to_string(&path)
            .map_err(|_| format!("release file not found at {}", path.display()))?
            .trim()
            .to_string();
        if name.is_empty() {
            return Err(format!("release file is empty: {}", path.display()));
        }
        Ok(name)
    }

    pub(crate) fn values_file(&self) -> PathBuf {
        self.root.join("values.yaml")
    }
}

pub(crate) fn resolve() -> Result<Scope, String> {
    let local_charts = PathBuf::from("./charts/sycophant");
    if local_charts.is_dir() {
        let scope = Scope {
            root: PathBuf::from("."),
        };
        crate::sync::auto_sync(&scope)?;
        return Ok(scope);
    }

    let home = env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let global_root = PathBuf::from(&home).join(".config").join("sycophant");
    if global_root.join("charts").join("sycophant").is_dir() {
        let scope = Scope { root: global_root };
        crate::sync::auto_sync(&scope)?;
        return Ok(scope);
    }

    Err("Not initialized. Run: syco init global  or  syco init local <name>".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charts_dir_path() {
        let scope = Scope {
            root: PathBuf::from("/home/user/.config/sycophant"),
        };
        assert_eq!(
            scope.charts_dir(),
            PathBuf::from("/home/user/.config/sycophant/charts/sycophant")
        );
    }

    #[test]
    fn examples_dir_path() {
        let scope = Scope {
            root: PathBuf::from("/tmp/project"),
        };
        assert_eq!(scope.examples_dir(), PathBuf::from("/tmp/project/examples"));
    }

    #[test]
    fn version_file_path() {
        let scope = Scope {
            root: PathBuf::from("."),
        };
        assert_eq!(scope.version_file(), PathBuf::from("./version"));
    }

    #[test]
    fn release_file_path() {
        let scope = Scope {
            root: PathBuf::from("/tmp/project"),
        };
        assert_eq!(scope.release_file(), PathBuf::from("/tmp/project/release"));
    }

    #[test]
    fn values_file_path() {
        let scope = Scope {
            root: PathBuf::from("/tmp/project"),
        };
        assert_eq!(
            scope.values_file(),
            PathBuf::from("/tmp/project/values.yaml")
        );
    }

    #[test]
    fn release_name_reads_file() {
        let dir = std::env::temp_dir().join("syco-scope-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("release"), "my-project\n").unwrap();
        let scope = Scope { root: dir.clone() };
        assert_eq!(scope.release_name().unwrap(), "my-project");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn release_name_errors_on_missing_file() {
        let scope = Scope {
            root: PathBuf::from("/nonexistent"),
        };
        assert!(scope.release_name().is_err());
    }

    #[test]
    fn release_name_errors_on_empty_file() {
        let dir = std::env::temp_dir().join("syco-scope-empty-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("release"), "  \n").unwrap();
        let scope = Scope { root: dir.clone() };
        assert!(scope.release_name().is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
