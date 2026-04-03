use crate::runner::run_passthrough;
use crate::scope::Scope;

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
