use crate::runner::{run_output, run_passthrough};
use crate::scope::Scope;

pub(crate) fn run(_scope: &Scope) -> Result<(), String> {
    run_passthrough("kubectl", &["cluster-info"])?;
    eprintln!();

    let args = &[
        "get",
        "pods",
        "-l",
        "app.kubernetes.io/part-of=sycophant",
        "-o",
        "wide",
    ];

    match run_output("kubectl", args) {
        Ok(text) if !text.is_empty() => {
            run_passthrough("kubectl", args)?;
        }
        _ => {
            eprintln!("No workspaces running.");
        }
    }

    Ok(())
}
