use std::env;
use std::path::PathBuf;

pub struct Scope {
    pub root: PathBuf,
}

impl Scope {
    pub fn charts_dir(&self) -> PathBuf {
        self.root.join("charts").join("sycophant")
    }

    pub fn examples_dir(&self) -> PathBuf {
        self.root.join("examples")
    }

    pub fn version_file(&self) -> PathBuf {
        self.root.join("version")
    }
}

pub fn resolve() -> Result<Scope, String> {
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

    Err("Not initialized. Run: syco init global  or  syco init local".into())
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
}
