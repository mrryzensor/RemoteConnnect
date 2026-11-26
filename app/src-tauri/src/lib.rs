use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use directories::ProjectDirs;
use remoteconnect_agent::{start_agent, AgentConfig, AgentEvent, AgentHandle};
use remoteconnect_protocol::{generate_device_id, generate_pin, SessionPermissions};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedDevice {
    pub id: String,
    pub name: String,
    pub pin: Option<String>,
    pub notes: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub device_id: String,
    pub device_name: String,
    pub pin: String,
    #[serde(default = "default_true")]
    pub pin_enabled: bool,
    #[serde(default)]
    pub custom_password: Option<String>,
    #[serde(default)]
    pub custom_password_enabled: bool,
    #[serde(default)]
    pub default_permissions: SessionPermissions,
    pub server_url: String,
    pub auth_token: Option<String>,
    #[serde(default)]
    pub saved_devices: Vec<SavedDevice>,
}

impl AppConfig {
    fn config_path() -> PathBuf {
        if let Some(proj) = ProjectDirs::from("com", "RemoteConnect", "RemoteConnect") {
            let dir = proj.config_dir();
            let _ = fs::create_dir_all(dir);
            return dir.join("config.json");
        }
        PathBuf::from("remoteconnect-config.json")
    }

    pub fn load_or_create() -> Self {
        let path = Self::config_path();
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<Self>(&raw) {
                return cfg;
            }
        }
        let default_name = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "My-Desktop".into());
        let cfg = Self {
            device_id: generate_device_id(),
            device_name: default_name,
            pin: generate_pin(),
            pin_enabled: true,
            custom_password: None,
            custom_password_enabled: false,
            default_permissions: SessionPermissions::default(),
            server_url: "wss://remote.2mail.us/ws".into(),
            auth_token: None,
            saved_devices: vec![],
        };
        cfg.save();
        cfg
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, json);
        }
    }
}

struct DesktopState {
    config: Mutex<AppConfig>,
    agent: Mutex<Option<AgentHandle>>,
    agent_status: Mutex<String>,
}

fn spawn_embedded_agent(app: &AppHandle, state: &DesktopState) {
    let cfg = state.config.lock().unwrap().clone();
    if let Some(old) = state.agent.lock().unwrap().take() {
        old.stop();
    }

    let agent_cfg = AgentConfig {
        server_url: cfg.server_url.clone(),
        device_id: cfg.device_id.clone(),
        device_name: cfg.device_name.clone(),
        pin: cfg.pin.clone(),
        pin_enabled: cfg.pin_enabled,
        custom_password: cfg.custom_password.clone(),
        custom_password_enabled: cfg.custom_password_enabled,
        default_permissions: cfg.default_permissions,
        auto_accept: false,
        auth_token: cfg.auth_token.clone(),
    };

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let handle = tauri::async_runtime::block_on(async move { start_agent(agent_cfg, ev_tx) });
    *state.agent.lock().unwrap() = Some(handle);
    *state.agent_status.lock().unwrap() = "connecting".into();

    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = ev_rx.recv().await {
            if let Some(st) = app_handle.try_state::<DesktopState>() {
                let status = match &ev {
                    AgentEvent::Connecting { .. } => Some("connecting"),
                    AgentEvent::Registered { .. } => Some("online"),
                    AgentEvent::Disconnected { .. } => Some("offline"),
                    _ => None,
                };
                if let Some(s) = status {
                    *st.agent_status.lock().unwrap() = s.into();
                }
            }
            let _ = app_handle.emit("agent-event", &ev);
        }
    });
}

#[tauri::command]
fn get_config(state: State<'_, DesktopState>) -> AppConfig {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
fn get_agent_status(state: State<'_, DesktopState>) -> String {
    state.agent_status.lock().unwrap().clone()
}

#[tauri::command]
fn update_server_settings(
    app: AppHandle,
    state: State<'_, DesktopState>,
    server_url: String,
    auth_token: Option<String>,
    device_name: String,
) -> AppConfig {
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.server_url = server_url;
        cfg.auth_token = auth_token.filter(|t| !t.trim().is_empty());
        cfg.device_name = device_name;
        cfg.save();
    }
    spawn_embedded_agent(&app, &state);
    state.config.lock().unwrap().clone()
}

#[tauri::command]
fn update_access_settings(
    state: State<'_, DesktopState>,
    pin: String,
    pin_enabled: bool,
    custom_password: Option<String>,
    custom_password_enabled: bool,
    default_permissions: SessionPermissions,
) -> AppConfig {
    let clean_pw = custom_password.filter(|p| !p.is_empty());
    let mut cfg = state.config.lock().unwrap();
    cfg.pin = pin.clone();
    cfg.pin_enabled = pin_enabled;
    cfg.custom_password = clean_pw.clone();
    cfg.custom_password_enabled = custom_password_enabled;
    cfg.default_permissions = default_permissions;
    cfg.save();

    if let Some(handle) = state.agent.lock().unwrap().as_ref() {
        handle.update_access(
            pin,
            pin_enabled,
            clean_pw,
            custom_password_enabled,
            default_permissions,
        );
    }
    cfg.clone()
}

#[tauri::command]
fn regenerate_pin(state: State<'_, DesktopState>) -> AppConfig {
    let new_pin = generate_pin();
    let mut cfg = state.config.lock().unwrap();
    cfg.pin = new_pin.clone();
    let pin_enabled = cfg.pin_enabled;
    let custom_pw = cfg.custom_password.clone();
    let custom_pw_enabled = cfg.custom_password_enabled;
    let perms = cfg.default_permissions;
    cfg.save();

    if let Some(handle) = state.agent.lock().unwrap().as_ref() {
        handle.update_access(new_pin, pin_enabled, custom_pw, custom_pw_enabled, perms);
    }
    cfg.clone()
}

#[tauri::command]
fn respond_incoming_request(
    state: State<'_, DesktopState>,
    session_id: String,
    accepted: bool,
    permissions: SessionPermissions,
) {
    if let Some(handle) = state.agent.lock().unwrap().as_ref() {
        handle.respond_approval(session_id, accepted, permissions);
    }
}

#[tauri::command]
fn disconnect_host_session(state: State<'_, DesktopState>, session_id: String) {
    if let Some(handle) = state.agent.lock().unwrap().as_ref() {
        handle.disconnect_session(session_id);
    }
}

#[tauri::command]
fn save_device(state: State<'_, DesktopState>, device: SavedDevice) -> Vec<SavedDevice> {
    let mut cfg = state.config.lock().unwrap();
    if let Some(existing) = cfg.saved_devices.iter_mut().find(|d| d.id == device.id) {
        *existing = device;
    } else {
        cfg.saved_devices.push(device);
    }
    cfg.save();
    cfg.saved_devices.clone()
}

#[tauri::command]
fn remove_device(state: State<'_, DesktopState>, device_id: String) -> Vec<SavedDevice> {
    let mut cfg = state.config.lock().unwrap();
    cfg.saved_devices.retain(|d| d.id != device_id);
    cfg.save();
    cfg.saved_devices.clone()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt::try_init();
    let initial_cfg = AppConfig::load_or_create();

    tauri::Builder::default()
        .manage(DesktopState {
            config: Mutex::new(initial_cfg),
            agent: Mutex::new(None),
            agent_status: Mutex::new("offline".into()),
        })
        .setup(|app| {
            let state = app.state::<DesktopState>();
            spawn_embedded_agent(app.handle(), &state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            get_agent_status,
            update_server_settings,
            update_access_settings,
            regenerate_pin,
            respond_incoming_request,
            disconnect_host_session,
            save_device,
            remove_device,
        ])
        .run(tauri::generate_context!())
        .expect("error while running RemoteConnect desktop application");
}
