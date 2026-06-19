use crate::runner::{run_output, run_passthrough};
use crate::scope::Scope;

pub(crate) fn run(scope: &Scope) -> Result<(), String> {
    let release = scope.release_name()?;
    let chart_dir = scope.tenant_chart_dir();
    let values_file = scope.values_file();

    if !values_file.exists() {
        return Err(format!(
            "values.yaml not found at {}",
            values_file.display()
        ));
    }

    validate_models(&release)?;

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

/// Preflight: refuse to deploy when no Model CRs exist in the namespace, since
/// the runtime would have nothing to route turns to. Models are now standalone
/// CRs (applied by `syco model set`), not Helm values. Tolerant of a cold
/// cluster where the CRDs aren't installed yet — a kubectl error skips the check
/// rather than blocking the first install.
fn validate_models(namespace: &str) -> Result<(), String> {
    match run_output(
        "kubectl",
        &[
            "get",
            "models.sycophant.md",
            "-n",
            namespace,
            "-o",
            "jsonpath={.items[*].metadata.name}",
        ],
    ) {
        Ok(out) => validate_models_output(&out),
        Err(_) => Ok(()),
    }
}

fn validate_models_output(jsonpath_out: &str) -> Result<(), String> {
    if jsonpath_out.trim().is_empty() {
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
    fn validate_models_output_empty_errors() {
        let err = validate_models_output("").unwrap_err();
        assert!(err.contains("No models configured"));
        assert!(validate_models_output("   ").is_err());
    }

    #[test]
    fn validate_models_output_nonempty_passes() {
        validate_models_output("anthropic.haiku default").unwrap();
    }
}
