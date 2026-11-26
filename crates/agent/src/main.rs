use clap::Parser;
use remoteconnect_agent::{start_agent, AgentConfig, AgentEvent};
use remoteconnect_protocol::{
    format_device_id, generate_device_id, generate_pin, SessionPermissions,
};
use tokio::sync::mpsc;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "remoteconnect-agent",
    about = "Standalone RemoteConnect Host Agent (Windows/Linux/macOS)"
)]
struct Args {
    /// WebSocket URL of the RemoteConnect Rust server
    #[arg(
        short,
        long,
        default_value = "ws://127.0.0.1:8080/ws",
        env = "REMOTECONNECT_SERVER"
    )]
    server: String,

    /// Custom 9-digit device ID (randomly generated if omitted)
    #[arg(long, env = "REMOTECONNECT_DEVICE_ID")]
    device_id: Option<String>,

    /// Human-readable device name
    #[arg(long, env = "REMOTECONNECT_DEVICE_NAME")]
    name: Option<String>,

    /// Unattended access PIN (randomly generated if omitted)
    #[arg(long, env = "REMOTECONNECT_PIN")]
    pin: Option<String>,

    /// Optional custom permanent password
    #[arg(long, env = "REMOTECONNECT_PASSWORD")]
    password: Option<String>,

    /// Auto-accept incoming connections without interactive confirmation
    #[arg(long, default_value_t = false)]
    auto_accept: bool,

    /// Optional server auth token
    #[arg(long, env = "REMOTECONNECT_AUTH_TOKEN")]
    auth_token: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let device_id = args.device_id.unwrap_or_else(generate_device_id);
    let pin = args.pin.unwrap_or_else(generate_pin);
    let custom_password_enabled = args.password.as_deref().map(|p| !p.is_empty()).unwrap_or(false);
    let device_name = args.name.unwrap_or_else(|| {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "RemoteConnect-Host".into())
    });

    println!("================================================");
    println!("  RemoteConnect Host Agent");
    println!("  Device ID : {}", format_device_id(&device_id));
    println!("  Access PIN: {}", pin);
    if custom_password_enabled {
        println!("  Password  : [CONFIGURED]");
    }
    println!("  Server    : {}", args.server);
    println!("================================================");

    let config = AgentConfig {
        server_url: args.server,
        device_id,
        device_name,
        pin,
        pin_enabled: true,
        custom_password: args.password,
        custom_password_enabled,
        default_permissions: SessionPermissions::default(),
        auto_accept: args.auto_accept,
        auth_token: args.auth_token,
    };

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let handle = start_agent(config, ev_tx);

    while let Some(event) = ev_rx.recv().await {
        match event {
            AgentEvent::Registered {
                device_id,
                server_version,
            } => {
                info!(
                    "Online! Device ID: {} (server v{})",
                    format_device_id(&device_id),
                    server_version
                );
            }
            AgentEvent::IncomingRequest {
                session_id,
                viewer_name,
            } => {
                info!(
                    "Incoming connection from '{}' (session {}). Rejecting (no PIN/password).",
                    viewer_name, session_id
                );
                handle.respond_approval(session_id, false, SessionPermissions::default());
            }
            AgentEvent::SessionStarted {
                session_id,
                viewer_name,
            } => {
                info!("Session {} started with '{}'", session_id, viewer_name);
            }
            AgentEvent::SessionEnded { session_id, reason } => {
                info!("Session {} ended: {}", session_id, reason);
            }
            AgentEvent::Disconnected { reason } => {
                info!("Disconnected: {}", reason);
            }
            AgentEvent::Error { message } => {
                info!("Agent error: {}", message);
            }
            AgentEvent::Connecting { .. } => {}
        }
    }

    Ok(())
}
