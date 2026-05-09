use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use lpa_core::{
    AppConfigLoader, FileSystemAppConfigLoader, FileSystemSkillCatalog, ModelCatalog,
    PresetModelCatalog, SkillsConfig,
};
use lpa_mcp::{McpManager, StdMcpManager, TrustLevel};
use lpa_tools::{MCP_TOOL_PREFIX, ToolRegistry, register_mcp_tools};
use lpa_utils::FileSystemConfigPathResolver;

use crate::{
    ListenTarget, ServerRuntime, execution::ServerRuntimeDependencies, load_server_provider,
    resolve_listen_targets, run_listeners,
};

/// Command-line arguments accepted by the standalone server process entrypoint.
#[derive(Debug, Clone, Parser)]
#[command(name = "lpa-server", version, about)]
pub struct ServerProcessArgs {
    /// Optional workspace root used for project-level config resolution.
    #[arg(long)]
    pub working_root: Option<PathBuf>,
}

/// Starts the transport-facing server runtime using the resolved application
/// configuration and listener set.
pub async fn run_server_process(args: ServerProcessArgs) -> Result<()> {
    let resolver = FileSystemConfigPathResolver::from_env()?;
    let loader = FileSystemAppConfigLoader::new(resolver.user_config_dir());
    let config = loader.load(args.working_root.as_deref())?;
    let listen_targets = resolve_listen_targets(&config.server.listen)?;
    let effective_listen = listen_targets
        .iter()
        .map(|target| match target {
            ListenTarget::Stdio => "stdio://".to_string(),
            ListenTarget::WebSocket { bind_address } => format!("ws://{bind_address}"),
        })
        .collect::<Vec<_>>();

    tracing::info!(
        user_config = %resolver.user_config_file().display(),
        project_config = args
            .working_root
            .as_deref()
            .map(|root| resolver.project_config_file(root).display().to_string())
            .unwrap_or_else(|| "<none>".into()),
        configured_listen = ?config.server.listen,
        effective_listen = ?effective_listen,
        max_connections = config.server.max_connections,
        "loaded server config"
    );

    let mut registry = ToolRegistry::new();
    lpa_tools::register_builtin_tools(&mut registry);

    // Spin up MCP supervisors + register any discovered tools into the registry.
    // Hold on to the concrete `StdMcpManager` so we can call its eager-start
    // helper (not part of the trait) and the final `shutdown_all`.
    let concrete_mcp_manager = Arc::new(StdMcpManager::from_config(&config.mcp)?);
    let mcp_manager: Arc<dyn McpManager> = Arc::clone(&concrete_mcp_manager) as Arc<dyn McpManager>;
    if config.mcp.auto_start {
        concrete_mcp_manager.start_configured(&config.mcp).await?;
    }
    let statuses = mcp_manager.statuses().await.unwrap_or_default();
    register_mcp_tools(&mut registry, Arc::clone(&mcp_manager), &statuses);

    // Collect tool names exposed by servers configured with `trust_level = "trusted"`.
    // These are pre-seeded into every new session's approval cache so trusted-server
    // tools skip the approval flow. `exposed_name` mirrors `McpToolAdapter::exposed_name`.
    let trusted_server_ids: std::collections::HashSet<_> = config
        .mcp
        .servers
        .iter()
        .filter(|s| matches!(s.trust_level, TrustLevel::Trusted))
        .map(|s| s.id.clone())
        .collect();
    let trusted_mcp_tool_names: Vec<String> = statuses
        .iter()
        .filter(|s| trusted_server_ids.contains(&s.server_id))
        .flat_map(|s| {
            s.tools
                .iter()
                .map(|t| format!("{MCP_TOOL_PREFIX}{}__{}", s.server_id, t.name))
        })
        .collect();

    let provider = load_server_provider(&resolver.user_config_file(), None)?;
    let model_catalog: Arc<dyn ModelCatalog> = Arc::new(PresetModelCatalog::load()?);
    let skill_workspace_root = args.working_root.clone();
    let project_skill_base = skill_workspace_root
        .as_deref()
        .map(|root| resolver.project_config_dir(root));
    let user_skill_roots = config
        .skills
        .user_roots
        .iter()
        .cloned()
        .map(|root| {
            if root.is_absolute() {
                root
            } else {
                resolver.user_config_dir().join(root)
            }
        })
        .collect();
    let workspace_skill_roots = config
        .skills
        .workspace_roots
        .iter()
        .cloned()
        .filter_map(|root| {
            if root.is_absolute() {
                Some(root)
            } else {
                project_skill_base.as_ref().map(|base| base.join(root))
            }
        })
        .collect();
    let skill_catalog = Box::new(FileSystemSkillCatalog::new(SkillsConfig {
        enabled: config.skills.enabled,
        user_roots: user_skill_roots,
        workspace_roots: workspace_skill_roots,
        watch_for_changes: config.skills.watch_for_changes,
    }));
    let runtime = ServerRuntime::new(
        resolver.user_config_dir(),
        ServerRuntimeDependencies::new(
            provider.provider,
            Arc::new(registry),
            provider.default_model,
            model_catalog,
            skill_workspace_root,
            skill_catalog,
            Arc::clone(&mcp_manager),
            trusted_mcp_tool_names,
        ),
    );
    tracing::info!("starting persisted session restore");
    runtime.load_persisted_sessions().await?;
    tracing::info!("persisted session restore completed");
    tracing::info!("server bootstrap completed; starting listeners");
    tokio::select! {
        result = run_listeners(runtime, &config.server.listen) => {
            result?;
        }
        result = tokio::signal::ctrl_c() => {
            result?;
            tracing::info!("server shutdown requested");
            concrete_mcp_manager.shutdown_all().await;
        }
    }
    Ok(())
}
