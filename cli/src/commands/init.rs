use std::io::Write;
use std::{env, fs, thread, time::Duration};

use crate::runner::{run_check, run_silent};

pub(crate) fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(|s| s.as_str()) {
        Some("global") => init_global(),
        Some("local") => init_local(),
        _ => Err("usage: syco init global|local".into()),
    }
}

fn init_global() -> Result<(), String> {
    let home = env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let root = std::path::PathBuf::from(&home)
        .join(".config")
        .join("sycophant");
    let agents_dir = std::path::PathBuf::from(&home)
        .join(".local")
        .join("share")
        .join("sycophant")
        .join("agents");

    if root.join("charts").join("sycophant").is_dir() {
        eprintln!(
            "already initialized. Global environment at {}",
            root.display()
        );
        return Ok(());
    }

    scaffold(&root)?;
    fs::create_dir_all(&agents_dir)
        .map_err(|e| format!("failed to create {}: {e}", agents_dir.display()))?;
    check_infra()?;
    eprintln!("sycophant initialized at {}", root.display());
    Ok(())
}

fn init_local() -> Result<(), String> {
    let root = std::path::PathBuf::from(".");
    if root.join("charts").join("sycophant").is_dir() {
        eprintln!("already initialized in current directory.");
        return Ok(());
    }

    scaffold(&root)?;
    fs::create_dir_all(root.join("agents"))
        .map_err(|e| format!("failed to create agents dir: {e}"))?;
    check_infra()?;
    eprintln!("sycophant initialized in current directory.");
    Ok(())
}

fn scaffold(root: &std::path::Path) -> Result<(), String> {
    let scope = crate::scope::Scope {
        root: root.to_path_buf(),
    };
    crate::sync::extract_assets(&scope)
}

fn check_infra() -> Result<(), String> {
    eprint!("Checking Docker... ");
    if !run_silent("docker", &["info"]) {
        eprintln!("not running");
        return Err("Docker Desktop is not running. Start it and run syco init again.".into());
    }
    eprintln!("ok");

    eprint!("Checking Kubernetes... ");
    if !run_silent("kubectl", &["cluster-info"]) {
        eprintln!("not ready");
        enable_kubernetes()?;
    } else {
        eprintln!("ok");
    }

    eprint!("Checking helm... ");
    if !run_silent("helm", &["version"]) {
        eprintln!("not found");
        return Err("helm is not installed. Run: brew install helm".into());
    }
    eprintln!("ok");

    Ok(())
}

fn enable_kubernetes() -> Result<(), String> {
    let home = env::var("HOME").map_err(|_| "HOME not set")?;
    let settings_path =
        format!("{home}/Library/Group Containers/group.com.docker/settings-store.json");

    eprintln!("Enabling Kubernetes in Docker Desktop...");

    let contents = fs::read_to_string(&settings_path)
        .map_err(|e| format!("failed to read Docker Desktop settings: {e}"))?;
    let mut settings: serde_json::Value =
        serde_json::from_str(&contents).map_err(|e| format!("failed to parse settings: {e}"))?;

    settings["KubernetesEnabled"] = serde_json::Value::Bool(true);

    let updated = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("failed to serialize settings: {e}"))?;
    fs::write(&settings_path, updated).map_err(|e| format!("failed to write settings: {e}"))?;

    eprintln!("Waiting for Kubernetes to start...");
    run_check("docker", &["desktop", "restart"])?;

    let mut elapsed = 0;
    let timeout: u64 = 120;
    let interval: u64 = 5;
    loop {
        if run_silent("kubectl", &["cluster-info"]) {
            eprintln!("Kubernetes is ready.");
            return Ok(());
        }
        elapsed += interval;
        if elapsed >= timeout {
            return Err("Kubernetes failed to start.".into());
        }
        eprint!(".");
        std::io::stderr().flush().ok();
        thread::sleep(Duration::from_secs(interval));
    }
}
