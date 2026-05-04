use crate::runner::run_passthrough;
use crate::scope::Scope;
use crate::values;

pub(crate) fn run(scope: &Scope) -> Result<(), String> {
    let release = scope.release_name()?;
    let chart_dir = scope.charts_dir();
    let values_file = scope.values_file();

    if !values_file.exists() {
        return Err(format!(
            "values.yaml not found at {}",
            values_file.display()
        ));
    }

    let root = values::load(&values_file)?;
    validate(&root)?;

    let chart_str = chart_dir.to_string_lossy().to_string();
    let values_str = values_file.to_string_lossy().to_string();

    eprintln!("Deploying {release}...");
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            &release,
            &chart_str,
            "-n",
            &release,
            "--create-namespace",
            "-f",
            &values_str,
        ],
    )
}

fn validate(root: &serde_yaml::Value) -> Result<(), String> {
    let models = root.get("models").and_then(|v| v.as_mapping());
    if models.is_none_or(|m| m.is_empty()) {
        return Err(
            "No models configured. Run: syco model set <model> --provider <provider> --secret <secret>"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_no_models_errors() {
        let root: serde_yaml::Value = serde_yaml::from_str("models: {}\n").unwrap();
        let err = validate(&root).unwrap_err();
        assert!(err.contains("No models configured"));
    }

    #[test]
    fn validate_minimal_models_passes() {
        let root: serde_yaml::Value = serde_yaml::from_str(
            "models:\n  anthropic.haiku:\n    format: anthropic\n    model: haiku\n    baseUrl: http://x\n",
        )
        .unwrap();
        validate(&root).unwrap();
    }
}
