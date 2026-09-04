use std::{ffi::OsString, path::PathBuf, time::Duration};

use clap::Parser;
use codex_acp_v2::{
    backend::{BackendExecutable, BackendOptions},
    server::{ServerOptions, run},
};
use tracing_subscriber::prelude::*;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "ACP protocol v2 agent backed by a Codex app-server child"
)]
struct Arguments {
    /// Override the bundled backend with a full Codex CLI executable.
    #[arg(long, env = "CODEX_PATH", conflicts_with = "app_server_path")]
    codex_path: Option<PathBuf>,
    /// Override the bundled backend with a standalone Codex app-server executable.
    #[arg(long, env = "CODEX_APP_SERVER_PATH", conflicts_with = "codex_path")]
    app_server_path: Option<PathBuf>,
    /// Backend argument; repeat as needed. Precedes standalone `--listen stdio://`
    /// or, with --codex-path, the full CLI's `app-server --stdio` subcommand.
    #[arg(long, allow_hyphen_values = true)]
    codex_arg: Vec<OsString>,
    /// Additional validated app-server initialize capabilities as a JSON object.
    #[arg(long, default_value = "{}", value_parser = |value: &str| serde_json::from_str::<serde_json::Value>(value))]
    backend_capabilities: serde_json::Value,
    /// Permit negotiated extensions for host/account/filesystem/process methods.
    #[arg(long)]
    allow_host_methods: bool,
    #[arg(long, default_value_t = 60)]
    request_timeout_seconds: u64,
    #[arg(long, default_value_t = 600)]
    interaction_timeout_seconds: u64,
    #[arg(long, default_value_t = 16_777_216)]
    max_frame_bytes: usize,
    #[arg(long, default_value_t = 64)]
    max_sessions: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(
                    // The SDK can log full packets and structured error data, including credentials.
                    tracing_subscriber::filter::filter_fn(|metadata| {
                        !metadata.target().starts_with("agent_client_protocol")
                    }),
                ),
        )
        .init();
    let args = Arguments::parse();
    anyhow::ensure!(
        args.request_timeout_seconds > 0
            && args.interaction_timeout_seconds > 0
            && args.max_sessions > 0,
        "timeouts and session limit must be positive"
    );
    run(ServerOptions {
        backend: BackendOptions {
            executable: args
                .codex_path
                .map(BackendExecutable::CodexCli)
                .or_else(|| args.app_server_path.map(BackendExecutable::AppServer))
                .unwrap_or(BackendExecutable::Bundled),
            args: args.codex_arg,
            request_timeout: Duration::from_secs(args.request_timeout_seconds),
            max_frame_bytes: args.max_frame_bytes,
            capabilities: args.backend_capabilities,
            ..BackendOptions::default()
        },
        allow_host_methods: args.allow_host_methods,
        max_sessions: args.max_sessions,
        interaction_timeout: Duration::from_secs(args.interaction_timeout_seconds),
        ..ServerOptions::default()
    })
    .await
}
