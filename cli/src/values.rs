use std::path::Path;

use serde_yaml::Value;

pub(crate) fn load(path: &Path) -> Result<Value, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_yaml::from_str(&content).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

pub(crate) fn save(path: &Path, value: &Value) -> Result<(), String> {
    let content =
        serde_yaml::to_string(value).map_err(|e| format!("failed to serialize values: {e}"))?;
    std::fs::write(path, content).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

pub(crate) fn ensure_map<'a>(root: &'a mut Value, key: &str) -> &'a mut serde_yaml::Mapping {
    if !root.get(key).is_some_and(|v| v.is_mapping()) {
        root[key] = Value::Mapping(serde_yaml::Mapping::new());
    }
    root[key].as_mapping_mut().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("syco-values-{}-{}", name, std::process::id()))
    }

    #[test]
    fn load_valid_yaml() {
        let path = tmp_path("load-valid");
        fs::write(&path, "models: {}\nagents: {}\n").unwrap();
        let val = load(&path).unwrap();
        assert!(val["models"].is_mapping());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn load_missing_file_errors() {
        let path = tmp_path("load-missing");
        assert!(load(&path).is_err());
    }

    #[test]
    fn load_invalid_yaml_errors() {
        let path = tmp_path("load-invalid");
        fs::write(&path, ": : : not yaml [[[").unwrap();
        assert!(load(&path).is_err());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn save_and_reload_roundtrip() {
        let path = tmp_path("roundtrip");
        let mut val = Value::Mapping(serde_yaml::Mapping::new());
        val["key"] = Value::String("value".into());
        save(&path, &val).unwrap();
        let reloaded = load(&path).unwrap();
        assert_eq!(reloaded["key"].as_str().unwrap(), "value");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn save_to_unwritable_path_errors() {
        let path = std::path::PathBuf::from("/nonexistent/dir/values.yaml");
        let val = Value::Mapping(serde_yaml::Mapping::new());
        assert!(save(&path, &val).is_err());
    }

    #[test]
    fn ensure_map_creates_missing_key() {
        let mut root = Value::Mapping(serde_yaml::Mapping::new());
        let map = ensure_map(&mut root, "models");
        assert!(map.is_empty());
    }

    #[test]
    fn ensure_map_preserves_existing_entries() {
        let mut root = Value::Mapping(serde_yaml::Mapping::new());
        let mut models = serde_yaml::Mapping::new();
        models.insert(
            Value::String("existing".into()),
            Value::String("data".into()),
        );
        root["models"] = Value::Mapping(models);
        let map = ensure_map(&mut root, "models");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn ensure_map_replaces_non_mapping_value() {
        let mut root = Value::Mapping(serde_yaml::Mapping::new());
        root["models"] = Value::String("not a map".into());
        let map = ensure_map(&mut root, "models");
        assert!(map.is_empty());
    }
}
