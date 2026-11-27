#[cfg(not(target_os = "android"))]
mod native_ui;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use directories::{ProjectDirs, UserDirs};
#[cfg(not(target_os = "android"))]
use remoteconnect_agent::{start_agent, AgentConfig, AgentEvent, AgentHandle};
use remoteconnect_protocol::{generate_device_id, generate_pin, SessionPermissions};
use serde::{Deserialize, Serialize};
#[cfg(not(target_os = "android"))]
use tauri::menu::{Menu, MenuItem};
#[cfg(not(target_os = "android"))]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
#[cfg(not(target_os = "android"))]
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};
#[cfg(target_os = "android")]
use tauri::{AppHandle, Manager, State};
#[cfg(not(target_os = "android"))]
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedDevice {
    pub id: String,
    pub name: String,
    pub pin: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveHostSessionInfo {
    pub session_id: String,
    pub viewer_name: String,
}

fn default_true() -> bool {
    true
}

fn default_fps() -> u8 {
    30
}

fn default_bitrate() -> u32 {
    2500
}

fn default_scale() -> f32 {
    1.0
}

fn default_ui_mode() -> String {
    "auto".into()
}

fn default_right_click_gesture() -> String {
    "long_press".into()
}

pub fn default_recordings_dir() -> String {
    if let Some(ud) = UserDirs::new() {
        if let Some(vid) = ud.video_dir() {
            return vid.join("RemoteConnect").to_string_lossy().to_string();
        }
        return ud
            .home_dir()
            .join("RemoteConnect-Recordings")
            .to_string_lossy()
            .to_string();
    }
    "RemoteConnect-Recordings".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreferences {
    #[serde(default = "default_recordings_dir")]
    pub recordings_dir: String,
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    #[serde(default = "default_true")]
    pub show_host_widget: bool,
    #[serde(default = "default_fps")]
    pub default_fps: u8,
    #[serde(default = "default_bitrate")]
    pub default_bitrate_kbps: u32,
    #[serde(default = "default_scale")]
    pub default_scale: f32,
    #[serde(default)]
    pub mute_audio_on_connect: bool,
    #[serde(default = "default_true")]
    pub prefer_p2p: bool,
    #[serde(default = "default_ui_mode")]
    pub ui_mode: String,
    #[serde(default = "default_right_click_gesture")]
    pub right_click_gesture: String,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            recordings_dir: default_recordings_dir(),
            minimize_to_tray: true,
            show_host_widget: true,
            default_fps: 30,
            default_bitrate_kbps: 2500,
            default_scale: 1.0,
            mute_audio_on_connect: false,
            prefer_p2p: true,
            ui_mode: default_ui_mode(),
            right_click_gesture: default_right_click_gesture(),
        }
    }
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
    #[serde(default)]
    pub autostart_initialized: bool,
    #[serde(default)]
    pub preferences: UserPreferences,
}

static CONFIG_DIR_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[cfg(target_os = "windows")]
fn deterministic_machine_device_id() -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_READ,
    };

    let subkey: Vec<u16> = "SOFTWARE\\Microsoft\\Cryptography"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let val_name: Vec<u16> = "MachineGuid"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut hkey = Default::default();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            return None;
        }
        let mut buf = [0u16; 128];
        let mut byte_len = (buf.len() * std::mem::size_of::<u16>()) as u32;
        let ok = RegQueryValueExW(
            hkey,
            PCWSTR(val_name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut byte_len),
        )
        .is_ok();
        let _ = RegCloseKey(hkey);
        if !ok || byte_len < 4 {
            return None;
        }
        let u16_len = (byte_len as usize) / 2;
        let guid = String::from_utf16_lossy(&buf[..u16_len])
            .trim_matches('\0')
            .trim()
            .to_string();
        if guid.is_empty() {
            return None;
        }
        // FNV-1a 64-bit hash -> stable 9-digit device ID (100_000_000..=999_999_999)
        let mut h: u64 = 0xcbf29ce484222325;
        for b in format!("RemoteConnect-DeviceID-v1:{}", guid).as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let nine_digits = 100_000_000 + (h % 900_000_000);
        Some(nine_digits.to_string())
    }
}

#[cfg(not(target_os = "windows"))]
fn deterministic_machine_device_id() -> Option<String> {
    None
}

impl AppConfig {
    fn config_path() -> PathBuf {
        if let Some(dir) = CONFIG_DIR_OVERRIDE.get() {
            let _ = fs::create_dir_all(dir);
            return dir.join("config.json");
        }
        if let Some(proj) = ProjectDirs::from("com", "RemoteConnect", "RemoteConnect") {
            let dir = proj.config_dir();
            let _ = fs::create_dir_all(dir);
            return dir.join("config.json");
        }
        PathBuf::from("remoteconnect-config.json")
    }

    pub fn load_or_create() -> Self {
        let path = Self::config_path();
        let bak_path = path.with_extension("json.bak");

        // 1. Try primary config.json, then automatic backup config.json.bak
        for candidate in [&path, &bak_path] {
            if let Ok(raw) = fs::read_to_string(candidate) {
                if let Ok(mut cfg) = serde_json::from_str::<Self>(&raw) {
                    // Self-heal if loaded from backup
                    if candidate == &bak_path {
                        cfg.save();
                    }
                    // Ensure non-empty device_id
                    if cfg.device_id.trim().is_empty() {
                        cfg.device_id = deterministic_machine_device_id()
                            .unwrap_or_else(generate_device_id);
                        cfg.save();
                    }
                    return cfg;
                }
            }
        }

        // 2. Preserve any unreadable file before creating a fresh config
        if path.exists() {
            let _ = fs::copy(&path, path.with_extension("json.corrupt"));
        }

        let default_name = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "My-Desktop".into());
        let cfg = Self {
            device_id: deterministic_machine_device_id().unwrap_or_else(generate_device_id),
            device_name: default_name,
            pin: generate_pin(),
            pin_enabled: true,
            custom_password: None,
            custom_password_enabled: false,
            default_permissions: SessionPermissions::default(),
            server_url: "wss://remote.2mail.us/ws".into(),
            auth_token: None,
            saved_devices: vec![],
            autostart_initialized: false,
            preferences: UserPreferences::default(),
        };
        cfg.save();
        cfg
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            // Atomic write (tmp -> replace) + backup copy so updates/crashes never corrupt config.json
            let tmp_path = path.with_extension("json.tmp");
            let bak_path = path.with_extension("json.bak");
            if fs::write(&tmp_path, &json).is_ok() {
                if path.exists() {
                    let _ = fs::copy(&path, &bak_path);
                }
                if fs::rename(&tmp_path, &path).is_err() {
                    let _ = fs::write(&path, &json);
                    let _ = fs::remove_file(&tmp_path);
                }
            }
        }
    }
}

struct DesktopState {
    config: Mutex<AppConfig>,
    #[cfg(not(target_os = "android"))]
    agent: Mutex<Option<AgentHandle>>,
    agent_status: Mutex<String>,
    active_sessions: Mutex<Vec<ActiveHostSessionInfo>>,
    viewer_active: AtomicBool,
}

// ============================================================
// Windows Auto-Start (HKCU\Software\Microsoft\Windows\CurrentVersion\Run)
// ============================================================

#[cfg(target_os = "windows")]
pub(crate) mod autostart {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
    };

    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE_NAME: &str = "RemoteConnect";

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn is_enabled() -> bool {
        unsafe {
            let subkey = to_wide(RUN_KEY);
            let mut hkey = Default::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                Some(0),
                KEY_READ,
                &mut hkey,
            )
            .is_err()
            {
                return false;
            }
            let name = to_wide(VALUE_NAME);
            let exists = RegQueryValueExW(
                hkey,
                PCWSTR(name.as_ptr()),
                None,
                None,
                None,
                None,
            )
            .is_ok();
            let _ = RegCloseKey(hkey);
            exists
        }
    }

    pub fn set_enabled(enabled: bool) -> Result<bool, String> {
        unsafe {
            let subkey = to_wide(RUN_KEY);
            let mut hkey = Default::default();
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                Some(0),
                KEY_WRITE,
                &mut hkey,
            )
            .ok()
            .map_err(|e| format!("RegOpenKeyExW failed: {:?}", e))?;

            let name = to_wide(VALUE_NAME);
            let result = if enabled {
                let exe = std::env::current_exe()
                    .map_err(|e| format!("current_exe failed: {}", e))?;
                let cmd = format!("\"{}\" --background", exe.display());
                let wide = to_wide(&cmd);
                let bytes = std::slice::from_raw_parts(
                    wide.as_ptr() as *const u8,
                    wide.len() * std::mem::size_of::<u16>(),
                );
                RegSetValueExW(hkey, PCWSTR(name.as_ptr()), Some(0), REG_SZ, Some(bytes))
                    .ok()
                    .map_err(|e| format!("RegSetValueExW failed: {:?}", e))
            } else {
                let _ = RegDeleteValueW(hkey, PCWSTR(name.as_ptr()));
                Ok(())
            };

            let _ = RegCloseKey(hkey);
            result.map(|_| enabled)
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod autostart {
    pub fn is_enabled() -> bool {
        false
    }
    pub fn set_enabled(_enabled: bool) -> Result<bool, String> {
        Ok(false)
    }
}

// ============================================================
// Windows WebView2 Runtime Check & Headless Fallback
// ============================================================

#[cfg(target_os = "windows")]
mod webview2_check {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_READ,
    };

    const WV2_KEYS: &[(HKEY, &str)] = &[
        (
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        ),
        (
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        ),
        (
            HKEY_CURRENT_USER,
            "Software\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        ),
    ];

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn is_installed() -> bool {
        unsafe {
            let pv_name = to_wide("pv");
            for (root, path) in WV2_KEYS {
                let subkey = to_wide(path);
                let mut hkey = Default::default();
                if RegOpenKeyExW(*root, PCWSTR(subkey.as_ptr()), Some(0), KEY_READ, &mut hkey)
                    .is_ok()
                {
                    let mut buf = [0u8; 64];
                    let mut len = buf.len() as u32;
                    let ok = RegQueryValueExW(
                        hkey,
                        PCWSTR(pv_name.as_ptr()),
                        None,
                        None,
                        Some(buf.as_mut_ptr()),
                        Some(&mut len),
                    )
                    .is_ok()
                        && len > 4;
                    let _ = RegCloseKey(hkey);
                    if ok {
                        return true;
                    }
                }
            }
            false
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod webview2_check {
    pub fn is_installed() -> bool {
        true
    }
}

// ============================================================
// Minimalist Host Session Floating Widget Window
// ============================================================

#[cfg(not(target_os = "android"))]
fn show_or_create_host_widget(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("host-widget") {
        let _ = win.show();
        return;
    }

    let width = 304.0;
    let height = 42.0;

    let builder = WebviewWindowBuilder::new(
        app,
        "host-widget",
        WebviewUrl::App("index.html?mode=widget".into()),
    )
    .title("RemoteConnect Sesión")
    .inner_size(width, height)
    .resizable(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .focused(false)
    .visible(true);

    if let Ok(win) = builder.build() {
        // Position unobtrusively at the bottom-right corner above the taskbar
        if let Ok(Some(monitor)) = win.current_monitor() {
            let size = monitor.size();
            let scale = monitor.scale_factor();
            let w_px = (width * scale) as i32;
            let h_px = (height * scale) as i32;
            let margin_x = (16.0 * scale) as i32;
            let margin_y = (58.0 * scale) as i32;
            let x = (size.width as i32 - w_px - margin_x).max(0);
            let y = (size.height as i32 - h_px - margin_y).max(0);
            let _ = win.set_position(PhysicalPosition::new(x, y));
        }
    }
}

#[cfg(not(target_os = "android"))]
fn hide_host_widget(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("host-widget") {
        let _ = win.hide();
    }
}

#[cfg(target_os = "android")]
fn hide_host_widget(_app: &AppHandle) {}

#[cfg(not(target_os = "android"))]
fn reveal_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

#[cfg(target_os = "android")]
fn reveal_main_window(_app: &AppHandle) {}

#[cfg(target_os = "android")]
fn spawn_embedded_agent(_app: &AppHandle, state: &DesktopState) {
    *state.agent_status.lock().unwrap() = "mobile-viewer".into();
}

#[cfg(not(target_os = "android"))]
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
                match &ev {
                    AgentEvent::Connecting { .. } => {
                        *st.agent_status.lock().unwrap() = "connecting".into();
                    }
                    AgentEvent::Registered { .. } => {
                        *st.agent_status.lock().unwrap() = "online".into();
                    }
                    AgentEvent::Disconnected { .. } => {
                        *st.agent_status.lock().unwrap() = "offline".into();
                    }
                    AgentEvent::IncomingRequest { .. } => {
                        // Bring main window to front only when manual approval is required
                        reveal_main_window(&app_handle);
                    }
                    AgentEvent::SessionStarted {
                        session_id,
                        viewer_name,
                    } => {
                        {
                            let mut list = st.active_sessions.lock().unwrap();
                            list.retain(|s| s.session_id != *session_id);
                            list.push(ActiveHostSessionInfo {
                                session_id: session_id.clone(),
                                viewer_name: viewer_name.clone(),
                            });
                        }
                        // Show ONLY the ultra-minimalist floating widget on the remote host
                        // (hide the full main window unless the user is actively viewing another PC)
                        let show_widget = st.config.lock().unwrap().preferences.show_host_widget;
                        if !st.viewer_active.load(Ordering::Relaxed) {
                            if let Some(main_win) = app_handle.get_webview_window("main") {
                                let _ = main_win.hide();
                            }
                        }
                        if show_widget {
                            show_or_create_host_widget(&app_handle);
                        }
                    }
                    AgentEvent::SessionEnded { session_id, .. } => {
                        let remaining = {
                            let mut list = st.active_sessions.lock().unwrap();
                            list.retain(|s| s.session_id != *session_id);
                            list.len()
                        };
                        if remaining == 0 {
                            // Keep the widget visible for 2.5s so the user clearly sees "Desconectado"
                            let app_for_hide = app_handle.clone();
                            tauri::async_runtime::spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                                if let Some(state_now) = app_for_hide.try_state::<DesktopState>() {
                                    if state_now.active_sessions.lock().unwrap().is_empty() {
                                        hide_host_widget(&app_for_hide);
                                    }
                                }
                            });
                        }
                    }
                    AgentEvent::Error { .. } => {}
                }
            }
            let _ = app_handle.emit("agent-event", &ev);
        }
    });
}

#[cfg(target_os = "windows")]
pub(crate) mod shortcut {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use directories::UserDirs;

    const CREATE_NO_WINDOW: u32 = 0x08000000;

    pub fn desktop_shortcut_path() -> Option<PathBuf> {
        UserDirs::new()
            .and_then(|u| u.desktop_dir().map(|d| d.join("RemoteConnect.lnk")))
    }

    pub fn exists() -> bool {
        desktop_shortcut_path().map(|p| p.exists()).unwrap_or(false)
    }

    pub fn create() -> Result<String, String> {
        let lnk = desktop_shortcut_path()
            .ok_or_else(|| "No se pudo encontrar la carpeta del Escritorio".to_string())?;
        let exe = std::env::current_exe()
            .map_err(|e| format!("No se encontró el ejecutable: {}", e))?;
        let work_dir = exe
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let lnk_str = lnk.to_string_lossy().replace('\'', "''");
        let exe_str = exe.to_string_lossy().replace('\'', "''");
        let dir_str = work_dir.replace('\'', "''");

        let ps = format!(
            "$ws = New-Object -ComObject WScript.Shell; \
             $s = $ws.CreateShortcut('{lnk_str}'); \
             $s.TargetPath = '{exe_str}'; \
             $s.WorkingDirectory = '{dir_str}'; \
             $s.IconLocation = '{exe_str},0'; \
             $s.Description = 'RemoteConnect — Escritorio Remoto E2E'; \
             $s.Save()"
        );

        let status = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| format!("Error creando acceso directo: {}", e))?;

        if !status.success() {
            return Err("No se pudo crear el acceso directo en el Escritorio".into());
        }
        Ok(lnk.to_string_lossy().to_string())
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) mod shortcut {
    pub fn exists() -> bool {
        true
    }
    pub fn create() -> Result<String, String> {
        Err("Solo disponible en Windows".into())
    }
}

#[tauri::command]
fn has_desktop_shortcut() -> bool {
    shortcut::exists()
}

#[tauri::command]
fn create_desktop_shortcut() -> Result<String, String> {
    shortcut::create()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestReleaseInfo {
    pub current_version: String,
    pub latest_tag: String,
    pub has_update: bool,
    pub windows_url: String,
    pub android_url: String,
}

fn parse_semver_tuple(v: &str) -> (u32, u32, u32) {
    let clean = v.trim().trim_start_matches(['v', 'V']).split('-').next().unwrap_or("");
    let mut parts = clean.split('.').map(|s| s.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

#[tauri::command]
async fn check_latest_release() -> Result<LatestReleaseInfo, String> {
    let current_version = env!("CARGO_PKG_VERSION").to_string();

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let tag = tokio::task::spawn_blocking(move || -> Result<String, String> {
            // Follow GitHub's /releases/latest redirect (no 60 req/hr API rate limit!)
            let out = std::process::Command::new("curl.exe")
                .args([
                    "-fsSLI",
                    "-o",
                    "NUL",
                    "-w",
                    "%{url_effective}",
                    "https://github.com/mrryzensor/RemoteConnnect/releases/latest",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|e| format!("Error consultando actualizaciones: {}", e))?;

            if !out.status.success() {
                return Err("No se pudo conectar con GitHub Releases".into());
            }
            let effective = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let tag = effective
                .rsplit('/')
                .next()
                .filter(|s| s.starts_with('v') || s.starts_with('V'))
                .ok_or_else(|| format!("No se pudo detectar versión desde {}", effective))?
                .to_string();
            Ok(tag)
        })
        .await
        .map_err(|e| format!("Error interno: {}", e))??;

        let has_update = parse_semver_tuple(&tag) > parse_semver_tuple(&current_version);
        let windows_url = format!(
            "https://github.com/mrryzensor/RemoteConnnect/releases/download/{}/RemoteConnect-Portable-x64.exe",
            tag
        );
        let android_url = format!(
            "https://github.com/mrryzensor/RemoteConnnect/releases/download/{}/RemoteConnect-Android-arm64.apk",
            tag
        );
        return Ok(LatestReleaseInfo {
            current_version,
            latest_tag: tag,
            has_update,
            windows_url,
            android_url,
        });
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("use_js_fallback".into())
    }
}

#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
async fn install_windows_update(
    app: AppHandle,
    download_url: String,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let url = download_url.clone();
        let current_exe =
            std::env::current_exe().map_err(|e| format!("No se encontró el ejecutable: {}", e))?;
        let temp_exe = std::env::temp_dir().join("RemoteConnect-update.exe");
        let temp_str = temp_exe.to_string_lossy().to_string();

        // Download latest release binary silently using built-in Windows curl.exe
        let dl_result = tokio::task::spawn_blocking(move || {
            let status = std::process::Command::new("curl.exe")
                .args(["-fsSL", "--retry", "2", "-o", &temp_str, &url])
                .creation_flags(CREATE_NO_WINDOW)
                .status()
                .map_err(|e| format!("Error ejecutando descarga: {}", e))?;
            if !status.success() {
                return Err(format!("Descarga fallida (código {:?})", status.code()));
            }
            let meta = fs::metadata(&temp_exe)
                .map_err(|e| format!("Archivo descargado inválido: {}", e))?;
            if meta.len() < 1_000_000 {
                let _ = fs::remove_file(&temp_exe);
                return Err("El archivo descargado es demasiado pequeño o está incompleto".into());
            }

            // Windows allows renaming a running .exe; move old aside and place new binary
            let old_exe = current_exe.with_extension("exe.old");
            let _ = fs::remove_file(&old_exe);
            fs::rename(&current_exe, &old_exe)
                .map_err(|e| format!("No se pudo reemplazar el ejecutable actual: {}", e))?;

            if let Err(copy_err) = fs::copy(&temp_exe, &current_exe) {
                // Rollback if copy fails
                let _ = fs::rename(&old_exe, &current_exe);
                return Err(format!("No se pudo copiar la nueva versión: {}", copy_err));
            }
            let _ = fs::remove_file(&temp_exe);
            Ok(current_exe)
        })
        .await
        .map_err(|e| format!("Error interno de actualización: {}", e))??;

        // Stop embedded agent cleanly before launching the updated binary
        if let Some(st) = app.try_state::<DesktopState>() {
            if let Some(agent) = st.agent.lock().unwrap().take() {
                agent.stop();
            }
        }

        let mut cmd = std::process::Command::new(&dl_result);
        if std::env::args().any(|a| a == "--background") {
            cmd.arg("--background");
        }
        let _ = cmd.spawn();
        app.exit(0);
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, download_url);
        Err("Actualización automática directa solo disponible en Windows".into())
    }
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
fn get_active_host_sessions(state: State<'_, DesktopState>) -> Vec<ActiveHostSessionInfo> {
    state.active_sessions.lock().unwrap().clone()
}

#[tauri::command]
fn get_autostart() -> bool {
    autostart::is_enabled()
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<bool, String> {
    autostart::set_enabled(enabled)
}

#[tauri::command]
fn set_viewer_mode(state: State<'_, DesktopState>, active: bool) {
    state.viewer_active.store(active, Ordering::Relaxed);
}

#[tauri::command]
fn show_main_window(app: AppHandle) {
    reveal_main_window(&app);
}

#[tauri::command]
fn hide_widget_window(app: AppHandle) {
    hide_host_widget(&app);
}

#[tauri::command]
fn update_preferences(
    state: State<'_, DesktopState>,
    mut preferences: UserPreferences,
) -> AppConfig {
    if preferences.recordings_dir.trim().is_empty() {
        preferences.recordings_dir = default_recordings_dir();
    }
    preferences.default_fps = preferences.default_fps.clamp(10, 60);
    preferences.default_bitrate_kbps = preferences.default_bitrate_kbps.clamp(500, 8000);
    preferences.default_scale = preferences.default_scale.clamp(0.5, 1.0);

    let mut cfg = state.config.lock().unwrap();
    cfg.preferences = preferences;
    cfg.save();
    cfg.clone()
}

#[tauri::command]
fn save_recording_file(
    state: State<'_, DesktopState>,
    file_name: String,
    data_b64: String,
) -> Result<String, String> {
    let dir_str = {
        let cfg = state.config.lock().unwrap();
        if cfg.preferences.recordings_dir.trim().is_empty() {
            default_recordings_dir()
        } else {
            cfg.preferences.recordings_dir.clone()
        }
    };
    let dir = PathBuf::from(&dir_str);
    fs::create_dir_all(&dir).map_err(|e| format!("No se pudo crear la carpeta: {}", e))?;

    let safe_name: String = file_name
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let full_path = dir.join(if safe_name.is_empty() {
        "RemoteConnect-recording.webm".to_string()
    } else {
        safe_name
    });

    let bytes = B64
        .decode(data_b64.as_bytes())
        .map_err(|e| format!("Error decodificando grabación: {}", e))?;
    fs::write(&full_path, bytes).map_err(|e| format!("Error guardando video: {}", e))?;
    Ok(full_path.to_string_lossy().to_string())
}

#[tauri::command]
fn open_folder(path: String) -> Result<(), String> {
    let dir = PathBuf::from(if path.trim().is_empty() {
        default_recordings_dir()
    } else {
        path
    });
    let _ = fs::create_dir_all(&dir);

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&dir)
            .spawn()
            .map_err(|e| format!("No se pudo abrir la carpeta: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&dir)
            .spawn()
            .map_err(|e| format!("No se pudo abrir la carpeta: {}", e))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&dir)
            .spawn()
            .map_err(|e| format!("No se pudo abrir la carpeta: {}", e))?;
    }
    Ok(())
}

#[tauri::command]
fn relaunch_with_ui_mode(app: AppHandle, state: State<'_, DesktopState>, ui_mode: String) {
    let normalized = match ui_mode.as_str() {
        "native" | "webview" | "auto" => ui_mode,
        _ => "auto".into(),
    };
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.preferences.ui_mode = normalized.clone();
        cfg.save();
    }
    #[cfg(not(target_os = "android"))]
    {
        if let Some(agent) = state.agent.lock().unwrap().take() {
            agent.stop();
        }
        if let Ok(exe) = std::env::current_exe() {
            let mut cmd = std::process::Command::new(exe);
            if normalized == "native" {
                cmd.arg("--native");
            } else if normalized == "webview" {
                cmd.arg("--webview");
            }
            let _ = cmd.spawn();
        }
        app.exit(0);
    }
    #[cfg(target_os = "android")]
    let _ = app;
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

    #[cfg(not(target_os = "android"))]
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
    #[cfg(not(target_os = "android"))]
    {
        let pin_enabled = cfg.pin_enabled;
        let custom_pw = cfg.custom_password.clone();
        let custom_pw_enabled = cfg.custom_password_enabled;
        let perms = cfg.default_permissions;
        if let Some(handle) = state.agent.lock().unwrap().as_ref() {
            handle.update_access(new_pin, pin_enabled, custom_pw, custom_pw_enabled, perms);
        }
    }
    cfg.save();
    cfg.clone()
}

#[tauri::command]
fn respond_incoming_request(
    state: State<'_, DesktopState>,
    session_id: String,
    accepted: bool,
    permissions: SessionPermissions,
) {
    #[cfg(not(target_os = "android"))]
    if let Some(handle) = state.agent.lock().unwrap().as_ref() {
        handle.respond_approval(session_id, accepted, permissions);
    }
    #[cfg(target_os = "android")]
    let _ = (state, session_id, accepted, permissions);
}

#[tauri::command]
fn disconnect_host_session(state: State<'_, DesktopState>, session_id: String) {
    #[cfg(not(target_os = "android"))]
    if let Some(handle) = state.agent.lock().unwrap().as_ref() {
        handle.disconnect_session(session_id);
    }
    #[cfg(target_os = "android")]
    let _ = (state, session_id);
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

    // Clean up leftover .exe.old from a previous automatic update on Windows
    #[cfg(target_os = "windows")]
    if let Ok(exe) = std::env::current_exe() {
        let old_exe = exe.with_extension("exe.old");
        if old_exe.exists() {
            let _ = fs::remove_file(&old_exe);
        }
    }

    #[allow(unused_mut)]
    let mut initial_cfg = AppConfig::load_or_create();

    #[cfg(not(target_os = "android"))]
    let start_in_background = {
        let args: Vec<String> = std::env::args().collect();
        let bg = args.iter().any(|a| a == "--background");
        let force_native_flag = args.iter().any(|a| a == "--native");
        let force_webview_flag = args.iter().any(|a| a == "--webview");

        // Enable Windows auto-start automatically on first launch (can be toggled anytime in UI)
        if !initial_cfg.autostart_initialized {
            let _ = autostart::set_enabled(true);
            initial_cfg.autostart_initialized = true;
            initial_cfg.save();
        } else if autostart::is_enabled() {
            // Keep registry path updated in case the portable exe was moved/replaced
            let _ = autostart::set_enabled(true);
        }

        // Determine whether to launch the native GPU UI (either by user preference/flag
        // or automatically when Microsoft Edge WebView2 Runtime is not installed).
        let use_native_ui = if force_native_flag {
            true
        } else if force_webview_flag {
            !webview2_check::is_installed()
        } else {
            initial_cfg.preferences.ui_mode == "native" || !webview2_check::is_installed()
        };

        if use_native_ui {
            native_ui::run_native_fallback(initial_cfg, bg);
            return;
        }
        bg
    };

    let builder = tauri::Builder::default()
        .manage(DesktopState {
            config: Mutex::new(initial_cfg),
            #[cfg(not(target_os = "android"))]
            agent: Mutex::new(None),
            agent_status: Mutex::new(
                if cfg!(target_os = "android") {
                    "mobile-viewer"
                } else {
                    "offline"
                }
                .into(),
            ),
            active_sessions: Mutex::new(vec![]),
            viewer_active: AtomicBool::new(false),
        })
        .setup(move |app| {
            // Only override config dir on Android (where ProjectDirs returns None).
            // On desktop, ProjectDirs is used consistently both before and inside Tauri setup.
            #[cfg(target_os = "android")]
            if let Ok(app_cfg_dir) = app.path().app_config_dir() {
                let _ = CONFIG_DIR_OVERRIDE.set(app_cfg_dir);
                let reloaded = AppConfig::load_or_create();
                let st = app.state::<DesktopState>();
                *st.config.lock().unwrap() = reloaded;
            }

            #[cfg(not(target_os = "android"))]
            {
                // 1. Setup System Tray icon + menu
                let open_item =
                    MenuItem::with_id(app, "open", "Abrir RemoteConnect", true, None::<&str>)?;
                let quit_item =
                    MenuItem::with_id(app, "quit", "Salir completamente", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

                let mut tray_builder = TrayIconBuilder::with_id("main-tray")
                    .tooltip("RemoteConnect — Disponible en segundo plano")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "open" => reveal_main_window(app),
                        "quit" => {
                            if let Some(st) = app.try_state::<DesktopState>() {
                                if let Some(agent) = st.agent.lock().unwrap().take() {
                                    agent.stop();
                                }
                            }
                            app.exit(0);
                        }
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            reveal_main_window(tray.app_handle());
                        }
                    });

                if let Some(icon) = app.default_window_icon().cloned() {
                    tray_builder = tray_builder.icon(icon);
                }
                let _ = tray_builder.build(app)?;

                // 2. Show main window only when NOT launched with --background
                if !start_in_background {
                    reveal_main_window(app.handle());
                }
            }

            // 3. Start embedded host agent on desktop (no-op on Android viewer)
            let state = app.state::<DesktopState>();
            spawn_embedded_agent(app.handle(), &state);
            Ok(())
        });

    #[cfg(not(target_os = "android"))]
    let builder = builder.on_window_event(|window, event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            if window.label() == "host-widget" {
                api.prevent_close();
                let _ = window.hide();
            } else if window.label() == "main" {
                let minimize = window
                    .app_handle()
                    .try_state::<DesktopState>()
                    .map(|st| st.config.lock().unwrap().preferences.minimize_to_tray)
                    .unwrap_or(true);
                if minimize {
                    api.prevent_close();
                    let _ = window.hide();
                } else if let Some(st) = window.app_handle().try_state::<DesktopState>() {
                    if let Some(agent) = st.agent.lock().unwrap().take() {
                        agent.stop();
                    }
                    window.app_handle().exit(0);
                }
            }
        }
    });

    builder
        .invoke_handler(tauri::generate_handler![
            check_latest_release,
            get_app_version,
            has_desktop_shortcut,
            create_desktop_shortcut,
            install_windows_update,
            get_config,
            get_agent_status,
            get_active_host_sessions,
            get_autostart,
            set_autostart,
            set_viewer_mode,
            show_main_window,
            hide_widget_window,
            update_preferences,
            save_recording_file,
            open_folder,
            relaunch_with_ui_mode,
            update_server_settings,
            update_access_settings,
            regenerate_pin,
            respond_incoming_request,
            disconnect_host_session,
            save_device,
            remove_device,
        ])
        .run(tauri::generate_context!())
        .expect("error while running RemoteConnect application");
}
