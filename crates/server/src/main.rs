mod handlers;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::{routing::get, Router};
use clap::Parser;
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::state::ServerState;

#[derive(Parser, Debug)]
#[command(
    name = "remoteconnect-server",
    about = "Self-hosted Rust rendezvous & E2E encrypted relay server for RemoteConnect"
)]
struct Args {
    /// Address and port to listen on
    #[arg(short, long, default_value = "0.0.0.0:8080", env = "REMOTECONNECT_BIND")]
    bind: SocketAddr,

    /// Optional shared secret token required by hosts and viewers
    #[arg(long, env = "REMOTECONNECT_AUTH_TOKEN")]
    auth_token: Option<String>,

    /// Optional path to serve the built web UI (e.g., app/dist)
    #[arg(long, env = "REMOTECONNECT_WEB_DIR")]
    web_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let state = ServerState::new(args.auth_token.clone());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let mut app = Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/devices/{id}/status", get(handlers::device_status))
        .route("/ws", get(handlers::ws_handler))
        .layer(cors)
        .with_state(state);

    if let Some(dir) = args.web_dir {
        if dir.exists() {
            info!("Serving web UI from {}", dir.display());
            app = app.fallback_service(ServeDir::new(dir));
        }
    }

    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    info!(
        "RemoteConnect Server listening on http://{} (WebSocket: ws://{}/ws)",
        args.bind, args.bind
    );
    if args.auth_token.is_some() {
        info!("Server authentication token is ENABLED");
    } else {
        info!("Server running in open mode (use --auth-token to restrict access)");
    }

    axum::serve(listener, app).await?;
    Ok(())
}
