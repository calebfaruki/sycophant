use std::path::PathBuf;
use std::{env, fs};

use crate::cli::{InitCmd, InitTarget};
use crate::runner::run_silent;
use crate::scope::Scope;

pub(crate) fn run(cmd: InitCmd) -> Result<(), String> {
    match cmd.target {
        InitTarget::Global(_) => init_global(),
        InitTarget::Local(_) => init_local(),
    }
}

fn init_global() -> Result<(), String> {
    let home = env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let root = PathBuf::from(&home).join(".config").join("sycophant");
    let scope = Scope { root: root.clone() };

    if scope.charts_dir().is_dir() {
        eprintln!("Already initialized at {}", root.display());
        return Ok(());
    }

    scaffold(&scope, "sycophant")?;
    check_infra()?;
    eprintln!("Initialized at {}", root.display());
    Ok(())
}

fn init_local() -> Result<(), String> {
    let name = env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .ok_or("cannot determine directory name")?;

    let root = PathBuf::from(".");
    let scope = Scope { root };

    if scope.charts_dir().is_dir() {
        eprintln!("Already initialized in current directory.");
        return Ok(());
    }

    scaffold(&scope, &name)?;
    check_infra()?;
    eprintln!("Initialized in current directory (release: {name}).");
    Ok(())
}

fn scaffold(scope: &Scope, release_name: &str) -> Result<(), String> {
    crate::sync::extract_assets(scope)?;

    fs::write(scope.release_file(), release_name)
        .map_err(|e| format!("failed to write release file: {e}"))?;

    let values_path = scope.values_file();
    if !values_path.exists() {
        fs::write(&values_path, SCAFFOLD_VALUES)
            .map_err(|e| format!("failed to write values.yaml: {e}"))?;
    }

    Ok(())
}

fn check_infra() -> Result<(), String> {
    eprint!("Checking Docker... ");
    if !run_silent("docker", &["info"]) {
        eprintln!("not running");
        return Err("Docker is not running. Start Docker and run syco init again.".into());
    }
    eprintln!("ok");

    eprint!("Checking Kubernetes... ");
    if !run_silent("kubectl", &["cluster-info"]) {
        eprintln!("not available");
        return Err(
            "Kubernetes is not available. Enable it in Docker Desktop and run syco init again."
                .into(),
        );
    }
    eprintln!("ok");

    eprint!("Checking Helm... ");
    if !run_silent("helm", &["version"]) {
        eprintln!("not found");
        return Err(
            "Helm is not installed. Install it (https://helm.sh/docs/intro/install/) and run syco init again."
                .into(),
        );
    }
    eprintln!("ok");

    eprint!("Checking grpcurl... ");
    if !run_silent("grpcurl", &["--version"]) {
        eprintln!("not found");
        return Err(
            "grpcurl is not installed. Install it (https://github.com/fullstorydev/grpcurl#installation) and run syco init again."
                .into(),
        );
    }
    eprintln!("ok");

    Ok(())
}

const SCAFFOLD_VALUES: &str = r#"# Sycophant values.yaml
# Edit this file, then run: syco up
models: {}
agents: {}
workspaces: {}
chambers: {}
channels: {}
"#;
