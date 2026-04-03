mod agent;
mod clients;
mod config;
mod discover;
mod message_source;
mod prompt;
mod router;
mod tool_router;
mod turn;

use std::collections::HashMap;

use config::TransponderConfig;
use message_source::MessageSource;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().json().with_target(false).init();

    let config = TransponderConfig::from_env().map_err(|e| format!("config error: {e}"))?;

    let mut tightbeam = clients::TightbeamClient::connect(&config.tightbeam_addr).await?;
    let mut tightbeam_subscribe = clients::TightbeamClient::connect(&config.tightbeam_addr).await?;
    tracing::info!(addr = %config.tightbeam_addr, "connected to tightbeam controller");

    let workspace = clients::ToolClient::connect_uds(&config.workspace_tools_socket).await?;
    tracing::info!(socket = %config.workspace_tools_socket.display(), "connected to workspace tools");

    let airlock = match &config.airlock_addr {
        Some(addr) => {
            let client = clients::ToolClient::connect_tcp(addr).await?;
            tracing::info!(addr = %addr, "connected to airlock controller");
            Some(client)
        }
        None => None,
    };

    let mut tool_router = tool_router::ToolRouter::new(airlock, workspace);
    tool_router.initialize().await?;

    let prompts = discover::discover_prompts(&config.prompt_dir).await?;

    let models = config
        .workspace_config_dir
        .as_ref()
        .map(|dir| load_models(dir))
        .transpose()?
        .unwrap_or_default();

    let mut source: Box<dyn MessageSource> = if config.use_stdin {
        tracing::info!("using stdin message source");
        Box::new(message_source::StdinMessageSource::new())
    } else {
        let stream = tightbeam_subscribe.subscribe().await?;
        tracing::info!("subscribed to tightbeam for inbound messages");
        Box::new(message_source::SubscribeMessageSource::new(stream))
    };

    if prompts.len() == 1 {
        let (name, system_prompt) = prompts.into_iter().next().unwrap();
        tracing::info!(agent = %name, "running single-agent mode");
        agent::run_single_agent(
            config.max_iterations,
            &mut tightbeam,
            &mut tool_router,
            source.as_mut(),
            &name,
            &system_prompt,
            &models,
        )
        .await?;
    } else {
        tracing::info!(agents = prompts.len(), "running multi-agent mode");
        router::run_multi_agent(
            config.max_iterations,
            &mut tightbeam,
            &mut tool_router,
            source.as_mut(),
            &prompts,
            &models,
        )
        .await?;
    }

    Ok(())
}

fn load_models(dir: &std::path::Path) -> Result<HashMap<String, String>, String> {
    let mut models = HashMap::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read workspace config dir {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read directory entry: {e}"))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("invalid filename: {}", path.display()))?
            .to_string();
        let model = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?
            .trim()
            .to_string();
        models.insert(name, model);
    }
    Ok(models)
}
