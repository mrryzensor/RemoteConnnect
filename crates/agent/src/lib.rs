pub mod audio;
pub mod capture;
pub mod clipboard;
#[cfg(target_os = "windows")]
mod win_gdi;
pub mod files;
pub mod input;
pub mod p2p;

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use remoteconnect_protocol::{
    E2eSession, EphemeralKeyPair, MouseBtn, SessionMessage, SessionPermissions, SignalMessage,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{connect_async_with_config, tungstenite::Message as WsMessage};
use tracing::{error, info, warn};

use crate::audio::spawn_audio_capture;
use crate::capture::{
    get_display_bounds, list_displays, CaptureOutcome, DisplayBounds, H264ScreenCapturer,
};
use crate::clipboard::{
    get_clipboard_files, get_clipboard_text, set_clipboard_files, set_clipboard_text,
};
use crate::files::{list_directory, prepare_file_download, UploadManager};
use crate::input::InputController;
use crate::p2p::P2pManager;

pub const PACKET_AUDIO_PCM16: u8 = 0x02;
pub const PACKET_VIDEO_H264: u8 = 0x04;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub server_url: String,
    pub device_id: String,
    pub device_name: String,
    pub pin: String,
    pub pin_enabled: bool,
    pub custom_password: Option<String>,
    pub custom_password_enabled: bool,
    pub default_permissions: SessionPermissions,
    pub auto_accept: bool,
    pub auth_token: Option<String>,
}

impl AgentConfig {
    pub fn unattended_enabled(&self) -> bool {
        (self.pin_enabled && !self.pin.is_empty())
            || (self.custom_password_enabled
                && self
                    .custom_password
                    .as_deref()
                    .map(|p| !p.is_empty())
                    .unwrap_or(false))
    }

    pub fn credential_matches(&self, provided: &str) -> bool {
        if provided.is_empty() {
            return false;
        }
        if self.pin_enabled && !self.pin.is_empty() && provided == self.pin {
            return true;
        }
        if self.custom_password_enabled {
            if let Some(pw) = &self.custom_password {
                if !pw.is_empty() && provided == pw {
                    return true;
                }
            }
        }
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    Connecting { server_url: String },
    Registered { device_id: String, server_version: String },
    Disconnected { reason: String },
    IncomingRequest { session_id: String, viewer_name: String },
    SessionStarted { session_id: String, viewer_name: String },
    SessionEnded { session_id: String, reason: String },
    Error { message: String },
}

enum AgentCommand {
    RespondApproval {
        session_id: String,
        accepted: bool,
        permissions: SessionPermissions,
    },
    DisconnectSession {
        session_id: String,
    },
    UpdateAccess {
        pin: String,
        pin_enabled: bool,
        custom_password: Option<String>,
        custom_password_enabled: bool,
        default_permissions: SessionPermissions,
    },
    Stop,
}

#[derive(Clone)]
pub struct AgentHandle {
    cmd_tx: mpsc::UnboundedSender<AgentCommand>,
}

impl AgentHandle {
    pub fn respond_approval(
        &self,
        session_id: String,
        accepted: bool,
        permissions: SessionPermissions,
    ) {
        let _ = self.cmd_tx.send(AgentCommand::RespondApproval {
            session_id,
            accepted,
            permissions,
        });
    }

    pub fn disconnect_session(&self, session_id: String) {
        let _ = self
            .cmd_tx
            .send(AgentCommand::DisconnectSession { session_id });
    }

    pub fn update_access(
        &self,
        pin: String,
        pin_enabled: bool,
        custom_password: Option<String>,
        custom_password_enabled: bool,
        default_permissions: SessionPermissions,
    ) {
        let _ = self.cmd_tx.send(AgentCommand::UpdateAccess {
            pin,
            pin_enabled,
            custom_password,
            custom_password_enabled,
            default_permissions,
        });
    }

    pub fn stop(&self) {
        let _ = self.cmd_tx.send(AgentCommand::Stop);
    }
}

/// Non-blocking input actions dispatched to a dedicated OS thread.
enum InputAction {
    MouseMove {
        x: f64,
        y: f64,
        bounds: DisplayBounds,
    },
    MouseButton {
        button: MouseBtn,
        pressed: bool,
        x: f64,
        y: f64,
        bounds: DisplayBounds,
    },
    MouseScroll {
        delta_x: i32,
        delta_y: i32,
    },
    KeyEvent {
        code: String,
        key: String,
        pressed: bool,
    },
    TriggerPaste,
}

fn spawn_input_worker() -> std::sync::mpsc::Sender<InputAction> {
    let (tx, rx) = std::sync::mpsc::channel::<InputAction>();
    std::thread::spawn(move || {
        let Ok(mut ctrl) = InputController::new() else {
            warn!("Failed to initialize InputController on dedicated thread");
            return;
        };
        while let Ok(action) = rx.recv() {
            match action {
                InputAction::MouseMove { x, y, bounds } => ctrl.move_mouse(x, y, bounds),
                InputAction::MouseButton {
                    button,
                    pressed,
                    x,
                    y,
                    bounds,
                } => ctrl.mouse_button(button, pressed, x, y, bounds),
                InputAction::MouseScroll { delta_x, delta_y } => {
                    ctrl.mouse_scroll(delta_x, delta_y)
                }
                InputAction::KeyEvent { code, key, pressed } => {
                    ctrl.key_event(&code, &key, pressed)
                }
                InputAction::TriggerPaste => ctrl.trigger_paste(),
            }
        }
    });
    tx
}

struct StreamSettings {
    fps: AtomicU8,
    bitrate_kbps: AtomicU32,
    display_index: AtomicUsize,
    scale_x100: AtomicU8,
    show_cursor: AtomicBool,
    force_keyframe: AtomicBool,
    active: Arc<AtomicBool>,
    cached_bounds: Mutex<DisplayBounds>,
    capture_thread: Mutex<Option<std::thread::Thread>>,
}

impl StreamSettings {
    fn new() -> Self {
        let initial_bounds = get_display_bounds(0).unwrap_or_default();
        Self {
            fps: AtomicU8::new(30),
            bitrate_kbps: AtomicU32::new(2500),
            display_index: AtomicUsize::new(0),
            scale_x100: AtomicU8::new(100),
            show_cursor: AtomicBool::new(false),
            force_keyframe: AtomicBool::new(true),
            active: Arc::new(AtomicBool::new(true)),
            cached_bounds: Mutex::new(initial_bounds),
            capture_thread: Mutex::new(None),
        }
    }

    fn wake_capture(&self) {
        if let Ok(guard) = self.capture_thread.lock() {
            if let Some(t) = guard.as_ref() {
                t.unpark();
            }
        }
    }

    fn bounds(&self) -> DisplayBounds {
        *self.cached_bounds.lock().unwrap()
    }

    fn refresh_bounds(&self, display_index: usize) {
        if let Ok(b) = get_display_bounds(display_index) {
            *self.cached_bounds.lock().unwrap() = b;
        }
    }
}

struct RunningSession {
    e2e: Arc<E2eSession>,
    permissions: SessionPermissions,
    settings: Arc<StreamSettings>,
    p2p: Arc<P2pManager>,
    upload_mgr: Arc<Mutex<UploadManager>>,
    last_clipboard_text: Arc<Mutex<String>>,
    last_clipboard_files: Arc<Mutex<Vec<String>>>,
    stop_tx: Option<oneshot::Sender<()>>,
}

/// Start the RemoteConnect host agent in a background Tokio task.
pub fn start_agent(
    config: AgentConfig,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
) -> AgentHandle {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(run_agent_loop(config, event_tx, cmd_rx));
    AgentHandle { cmd_tx }
}

async fn run_agent_loop(
    mut config: AgentConfig,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<AgentCommand>,
) {
    loop {
        let _ = event_tx.send(AgentEvent::Connecting {
            server_url: config.server_url.clone(),
        });

        // disable_nagle = true (TCP_NODELAY) for minimum input & frame latency
        let connect_res = tokio::time::timeout(
            Duration::from_secs(12),
            connect_async_with_config(&config.server_url, None, true),
        )
        .await;

        match connect_res {
            Ok(Ok((ws_stream, _))) => {
                info!("Connected to server {}", config.server_url);
                let should_stop =
                    handle_connection(ws_stream, &mut config, &event_tx, &mut cmd_rx).await;
                if should_stop {
                    info!("Agent stopped by command");
                    return;
                }
                let _ = event_tx.send(AgentEvent::Disconnected {
                    reason: "Conexión perdida con el servidor, reconectando...".into(),
                });
            }
            Ok(Err(e)) => {
                let msg = format!("Connection to {} failed: {}", config.server_url, e);
                warn!("{}", msg);
                let _ = event_tx.send(AgentEvent::Disconnected { reason: msg });
            }
            Err(_) => {
                let msg = format!("Connection to {} timed out", config.server_url);
                warn!("{}", msg);
                let _ = event_tx.send(AgentEvent::Disconnected { reason: msg });
            }
        }

        let sleep = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(sleep);
        tokio::select! {
            _ = &mut sleep => {}
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(AgentCommand::Stop) | None => return,
                    Some(AgentCommand::UpdateAccess {
                        pin,
                        pin_enabled,
                        custom_password,
                        custom_password_enabled,
                        default_permissions,
                    }) => {
                        config.pin = pin;
                        config.pin_enabled = pin_enabled;
                        config.custom_password = custom_password;
                        config.custom_password_enabled = custom_password_enabled;
                        config.default_permissions = default_permissions;
                    }
                    _ => {}
                }
            }
        }
    }
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct PendingApproval {
    viewer_name: String,
    viewer_public_key: String,
}

async fn handle_connection(
    ws_stream: WsStream,
    config: &mut AgentConfig,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    cmd_rx: &mut mpsc::UnboundedReceiver<AgentCommand>,
) -> bool {
    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    // High-priority control/input/audio channel
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<WsMessage>();
    // Bounded video channel (capacity 1) to eliminate bufferbloat & queuing latency
    let (video_tx, mut video_rx) = mpsc::channel::<WsMessage>(1);

    let writer = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                biased;
                m = out_rx.recv() => match m {
                    Some(m) => m,
                    None => break,
                },
                v = video_rx.recv() => match v {
                    Some(m) => m,
                    None => break,
                },
            };
            if ws_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    let reg = SignalMessage::RegisterHost {
        device_id: config.device_id.clone(),
        device_name: config.device_name.clone(),
        os: std::env::consts::OS.to_string(),
        auth_token: config.auth_token.clone(),
        unattended_enabled: config.unattended_enabled(),
    };
    send_ws_signal(&out_tx, &reg);

    let mut sessions: HashMap<String, RunningSession> = HashMap::new();
    let mut pending: HashMap<String, PendingApproval> = HashMap::new();
    let input_tx = spawn_input_worker();
    let (p2p_in_tx, mut p2p_in_rx) = mpsc::unbounded_channel::<(String, SessionMessage)>();

    let mut ping_interval = tokio::time::interval(Duration::from_secs(15));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ping_ticks: u64 = 0;
    let mut last_server_activity = Instant::now();
    let mut stop_requested = false;

    loop {
        tokio::select! {
            Some((sid, smsg)) = p2p_in_rx.recv() => {
                if let Some(sess) = sessions.get(&sid) {
                    handle_session_message(&sid, smsg, sess, &input_tx, &out_tx);
                }
            }

            _ = ping_interval.tick() => {
                if writer.is_finished() {
                    warn!("WebSocket writer closed unexpectedly; forcing reconnect");
                    break;
                }
                if last_server_activity.elapsed() > Duration::from_secs(40) {
                    warn!("No server heartbeat/Pong for >40s (stale socket); forcing reconnect");
                    break;
                }
                ping_ticks += 1;
                let ts = chrono::Utc::now().timestamp_millis() as u64;
                send_ws_signal(&out_tx, &SignalMessage::Ping { timestamp: ts });
                let _ = out_tx.send(WsMessage::Ping(Vec::new().into()));

                // RustDesk-style periodic registration heartbeat (every 45s):
                // Guarantees this host stays registered on the server even if an older
                // socket cleanup or transient server state glitch ever occurred.
                if ping_ticks % 3 == 0 {
                    let refresh = SignalMessage::RegisterHost {
                        device_id: config.device_id.clone(),
                        device_name: config.device_name.clone(),
                        os: std::env::consts::OS.to_string(),
                        auth_token: config.auth_token.clone(),
                        unattended_enabled: config.unattended_enabled(),
                    };
                    send_ws_signal(&out_tx, &refresh);
                }
            }

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(AgentCommand::Stop) | None => {
                        stop_requested = true;
                        break;
                    }
                    Some(AgentCommand::UpdateAccess {
                        pin,
                        pin_enabled,
                        custom_password,
                        custom_password_enabled,
                        default_permissions,
                    }) => {
                        config.pin = pin;
                        config.pin_enabled = pin_enabled;
                        config.custom_password = custom_password;
                        config.custom_password_enabled = custom_password_enabled;
                        config.default_permissions = default_permissions;
                    }
                    Some(AgentCommand::DisconnectSession { session_id }) => {
                        if let Some(mut s) = sessions.remove(&session_id) {
                            s.settings.active.store(false, Ordering::Relaxed);
                            s.p2p.close();
                            if let Some(tx) = s.stop_tx.take() { let _ = tx.send(()); }
                        }
                        send_ws_signal(&out_tx, &SignalMessage::Disconnect {
                            session_id: session_id.clone(),
                            reason: Some("Host ended session".into()),
                        });
                        let _ = event_tx.send(AgentEvent::SessionEnded {
                            session_id,
                            reason: "Ended by host".into(),
                        });
                    }
                    Some(AgentCommand::RespondApproval { session_id, accepted, permissions }) => {
                        if let Some(req) = pending.remove(&session_id) {
                            if accepted {
                                start_session(
                                    session_id,
                                    req.viewer_name,
                                    &req.viewer_public_key,
                                    permissions,
                                    &out_tx,
                                    &video_tx,
                                    &p2p_in_tx,
                                    event_tx,
                                    &mut sessions,
                                );
                            } else {
                                send_ws_signal(&out_tx, &SignalMessage::ConnectResponse {
                                    session_id,
                                    accepted: false,
                                    reason: Some("Conexión rechazada por el anfitrión".into()),
                                    host_public_key: None,
                                    displays: vec![],
                                    permissions: SessionPermissions::default(),
                                });
                            }
                        }
                    }
                }
            }

            incoming = ws_rx.next() => {
                let Some(msg_res) = incoming else {
                    break;
                };
                let Ok(ws_msg) = msg_res else {
                    break;
                };

                last_server_activity = Instant::now();

                match ws_msg {
                    WsMessage::Ping(payload) => {
                        let _ = out_tx.send(WsMessage::Pong(payload));
                        continue;
                    }
                    WsMessage::Pong(_) => {
                        continue;
                    }
                    WsMessage::Close(_) => {
                        break;
                    }
                    WsMessage::Text(text) => {
                        let Ok(signal) = serde_json::from_str::<SignalMessage>(&text) else {
                            continue;
                        };

                        match signal {
                            SignalMessage::Registered { device_id, server_version } => {
                                info!("Registered as {} on server v{}", device_id, server_version);
                                let _ = event_tx.send(AgentEvent::Registered { device_id, server_version });
                            }

                        SignalMessage::IncomingConnection {
                            session_id,
                            viewer_name,
                            pin,
                            viewer_public_key,
                        } => {
                            let cred_ok = pin
                                .as_deref()
                                .map(|c| config.credential_matches(c))
                                .unwrap_or(false);

                            if cred_ok || config.auto_accept {
                                info!(session_id = %session_id, viewer = %viewer_name, "Auto-accepting session (PIN/Password/auto-accept)");
                                start_session(
                                    session_id,
                                    viewer_name,
                                    &viewer_public_key,
                                    config.default_permissions,
                                    &out_tx,
                                    &video_tx,
                                    &p2p_in_tx,
                                    event_tx,
                                    &mut sessions,
                                );
                            } else if pin.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
                                && config.unattended_enabled()
                            {
                                send_ws_signal(&out_tx, &SignalMessage::ConnectResponse {
                                    session_id,
                                    accepted: false,
                                    reason: Some("Contraseña o PIN incorrecto".into()),
                                    host_public_key: None,
                                    displays: vec![],
                                    permissions: SessionPermissions::default(),
                                });
                            } else {
                                pending.insert(session_id.clone(), PendingApproval {
                                    viewer_name: viewer_name.clone(),
                                    viewer_public_key,
                                });
                                let _ = event_tx.send(AgentEvent::IncomingRequest {
                                    session_id,
                                    viewer_name,
                                });
                            }
                        }

                        SignalMessage::EncryptedRelay { session_id, payload } => {
                            if let Some(sess) = sessions.get(&session_id) {
                                if let Ok(plain) = sess.e2e.decrypt_b64(&payload) {
                                    if let Ok(smsg) = serde_json::from_slice::<SessionMessage>(&plain) {
                                        handle_session_message(
                                            &session_id,
                                            smsg,
                                            sess,
                                            &input_tx,
                                            &out_tx,
                                        );
                                    }
                                }
                            }
                        }

                        SignalMessage::Disconnect { session_id, reason } => {
                            if let Some(mut s) = sessions.remove(&session_id) {
                                s.settings.active.store(false, Ordering::Relaxed);
                                s.p2p.close();
                                if let Some(tx) = s.stop_tx.take() { let _ = tx.send(()); }
                            }
                            pending.remove(&session_id);
                            let _ = event_tx.send(AgentEvent::SessionEnded {
                                session_id,
                                reason: reason.unwrap_or_else(|| "Disconnected".into()),
                            });
                        }

                        SignalMessage::Error { code, message } => {
                            error!("Server error [{}]: {}", code, message);
                            let _ = event_tx.send(AgentEvent::Error { message });
                        }

                        _ => {}
                    }
                    }
                    _ => {}
                }
            }
        }
    }

    for (sid, mut sess) in sessions.drain() {
        sess.settings.active.store(false, Ordering::Relaxed);
        sess.p2p.close();
        if let Some(tx) = sess.stop_tx.take() {
            let _ = tx.send(());
        }
        let _ = event_tx.send(AgentEvent::SessionEnded {
            session_id: sid,
            reason: "Server connection closed".into(),
        });
    }

    writer.abort();
    if !stop_requested {
        let _ = event_tx.send(AgentEvent::Disconnected {
            reason: "Connection lost, reconnecting...".into(),
        });
    }
    stop_requested
}

#[allow(clippy::too_many_arguments)]
fn start_session(
    session_id: String,
    viewer_name: String,
    viewer_public_key: &str,
    permissions: SessionPermissions,
    out_tx: &mpsc::UnboundedSender<WsMessage>,
    video_tx: &mpsc::Sender<WsMessage>,
    p2p_in_tx: &mpsc::UnboundedSender<(String, SessionMessage)>,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    sessions: &mut HashMap<String, RunningSession>,
) {
    let kp = EphemeralKeyPair::generate();
    let host_pub = kp.public_key_b64();
    let e2e = match kp.derive_session(viewer_public_key, &session_id) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            error!("E2E key derivation failed: {}", e);
            send_ws_signal(
                out_tx,
                &SignalMessage::ConnectResponse {
                    session_id,
                    accepted: false,
                    reason: Some("E2E handshake failed".into()),
                    host_public_key: None,
                    displays: vec![],
                    permissions,
                },
            );
            return;
        }
    };

    let displays = list_displays();
    send_ws_signal(
        out_tx,
        &SignalMessage::ConnectResponse {
            session_id: session_id.clone(),
            accepted: true,
            reason: None,
            host_public_key: Some(host_pub),
            displays,
            permissions,
        },
    );

    let settings = Arc::new(StreamSettings::new());
    let p2p = {
        let sid_ctrl = session_id.clone();
        let p2p_tx = p2p_in_tx.clone();
        let settings_kf = settings.clone();
        P2pManager::spawn(
            session_id.clone(),
            e2e.clone(),
            out_tx.clone(),
            move |smsg| {
                let _ = p2p_tx.send((sid_ctrl.clone(), smsg));
            },
            move || {
                settings_kf.force_keyframe.store(true, Ordering::Release);
                settings_kf.wake_capture();
            },
        )
    };
    let last_clipboard_text = Arc::new(Mutex::new(get_clipboard_text().unwrap_or_default()));
    let initial_files: Vec<String> = get_clipboard_files()
        .into_iter()
        .map(|f| f.path)
        .collect();
    let last_clipboard_files = Arc::new(Mutex::new(initial_files));
    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

    // 1. Spawn remote audio capture & streaming task if permitted (strictly P2P — zero server relay)
    if permissions.allow_audio {
        let active = settings.active.clone();
        let p2p_audio = p2p.clone();
        let mut audio_rx = spawn_audio_capture(active.clone());

        tokio::spawn(async move {
            while let Some(chunk) = audio_rx.recv().await {
                if !active.load(Ordering::Relaxed) {
                    break;
                }
                if !p2p_audio.is_media_ready() {
                    continue;
                }
                // Silence gate: skip sending dead-silent PCM (~1.5 Mbps saved when no audio plays)
                let has_sound = chunk
                    .pcm_i16_le
                    .chunks_exact(2)
                    .any(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs() >= 18);
                if !has_sound {
                    continue;
                }
                let mut plain = Vec::with_capacity(7 + chunk.pcm_i16_le.len());
                plain.push(PACKET_AUDIO_PCM16);
                plain.extend_from_slice(&chunk.sample_rate.to_be_bytes());
                plain.extend_from_slice(&chunk.channels.to_be_bytes());
                plain.extend_from_slice(&chunk.pcm_i16_le);
                let _ = p2p_audio.try_send_media(plain);
            }
        });
    }

    // 2. Spawn dedicated OS thread for H.264 capture + encoding (strictly P2P — server is signaling-only)
    {
        let _ = video_tx;
        let settings = settings.clone();
        let p2p_video = p2p.clone();

        std::thread::spawn(move || {
            if let Ok(mut guard) = settings.capture_thread.lock() {
                *guard = Some(std::thread::current());
            }
            let mut capturer = H264ScreenCapturer::new(0);
            let session_start = Instant::now();

            while settings.active.load(Ordering::Relaxed) {
                if !p2p_video.is_media_ready() {
                    std::thread::park_timeout(Duration::from_millis(25));
                    continue;
                }
                // Backpressure: if P2P channel hasn't drained previous frame, wait briefly before encoding
                if p2p_video.media_capacity() == 0 {
                    std::thread::park_timeout(Duration::from_millis(3));
                    continue;
                }

                let frame_start = Instant::now();
                let fps = settings.fps.load(Ordering::Relaxed).clamp(5, 60);
                let bitrate_kbps = settings.bitrate_kbps.load(Ordering::Relaxed);
                let display_idx = settings.display_index.load(Ordering::Relaxed);
                let scale = (settings.scale_x100.load(Ordering::Relaxed) as f32) / 100.0;
                let show_cursor = settings.show_cursor.load(Ordering::Relaxed);
                let force_kf = settings.force_keyframe.swap(false, Ordering::AcqRel);
                let ts_ms = session_start.elapsed().as_millis() as u32;

                match capturer.capture_and_encode(
                    display_idx,
                    scale,
                    bitrate_kbps,
                    fps,
                    force_kf,
                    show_cursor,
                    ts_ms,
                ) {
                    Ok(CaptureOutcome::Encoded(encoded)) => {
                        // Plaintext: [0x04][1B is_keyframe][4B BE w][4B BE h][4B BE ts_ms][H.264 Annex-B]
                        let mut plain = Vec::with_capacity(14 + encoded.h264_annex_b.len());
                        plain.push(PACKET_VIDEO_H264);
                        plain.push(if encoded.is_keyframe { 1 } else { 0 });
                        plain.extend_from_slice(&encoded.width.to_be_bytes());
                        plain.extend_from_slice(&encoded.height.to_be_bytes());
                        plain.extend_from_slice(&ts_ms.to_be_bytes());
                        plain.extend_from_slice(&encoded.h264_annex_b);

                        if p2p_video.is_media_ready() {
                            // Direct P2P (DTLS-encrypted UDP/SCTP) — zero traffic through relay server!
                            let _ = p2p_video.send_media_blocking(plain);
                        }

                        let budget = Duration::from_millis(1000 / (fps as u64));
                        let elapsed = frame_start.elapsed();
                        if elapsed < budget {
                            std::thread::park_timeout(budget - elapsed);
                        }
                    }
                    Ok(CaptureOutcome::Unchanged { waited_event }) => {
                        // If DXGI already waited for DWM events, loop immediately with zero extra latency.
                        // On polling fallback (xcap), sleep briefly (8ms) so new UI changes are caught fast.
                        if !waited_event {
                            let idle_sleep = Duration::from_millis(8);
                            let elapsed = frame_start.elapsed();
                            if elapsed < idle_sleep {
                                std::thread::park_timeout(idle_sleep - elapsed);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Capture/encode error: {}", e);
                        std::thread::park_timeout(Duration::from_millis(40));
                    }
                }
            }
        });
    }

    // 3. Lightweight clipboard polling task (1s interval, completely decoupled from video)
    if permissions.allow_clipboard {
        let sid = session_id.clone();
        let e2e = e2e.clone();
        let settings = settings.clone();
        let out_tx = out_tx.clone();
        let last_cb_text = last_clipboard_text.clone();
        let last_cb_files = last_clipboard_files.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(1000));
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = interval.tick() => {
                        if !settings.active.load(Ordering::Relaxed) {
                            break;
                        }
                        if permissions.allow_file_transfer {
                            let files = get_clipboard_files();
                            if !files.is_empty() {
                                let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
                                let mut guard = last_cb_files.lock().unwrap();
                                if *guard != paths {
                                    *guard = paths;
                                    drop(guard);
                                    send_encrypted_session_msg(
                                        &out_tx,
                                        &sid,
                                        &e2e,
                                        &SessionMessage::ClipboardFilesAvailable { files },
                                    );
                                }
                            }
                        }
                        if let Some(current) = get_clipboard_text() {
                            let mut guard = last_cb_text.lock().unwrap();
                            if !current.is_empty() && *guard != current {
                                *guard = current.clone();
                                drop(guard);
                                send_encrypted_session_msg(
                                    &out_tx,
                                    &sid,
                                    &e2e,
                                    &SessionMessage::ClipboardSync { text: current },
                                );
                            }
                        }
                    }
                }
            }
        });
    }

    sessions.insert(
        session_id.clone(),
        RunningSession {
            e2e,
            permissions,
            settings,
            p2p,
            upload_mgr: Arc::new(Mutex::new(UploadManager::default())),
            last_clipboard_text,
            last_clipboard_files,
            stop_tx: Some(stop_tx),
        },
    );

    let _ = event_tx.send(AgentEvent::SessionStarted {
        session_id,
        viewer_name,
    });
}

fn handle_session_message(
    session_id: &str,
    msg: SessionMessage,
    sess: &RunningSession,
    input_tx: &std::sync::mpsc::Sender<InputAction>,
    out_tx: &mpsc::UnboundedSender<WsMessage>,
) {
    match msg {
        SessionMessage::StreamConfig {
            fps,
            quality,
            display_index,
            scale,
            bitrate_kbps,
            show_cursor,
        } => {
            let prev_disp = sess.settings.display_index.swap(display_index, Ordering::Relaxed);
            if prev_disp != display_index {
                sess.settings.refresh_bounds(display_index);
                sess.settings.force_keyframe.store(true, Ordering::Release);
            }
            sess.settings.show_cursor.store(show_cursor, Ordering::Relaxed);
            sess.settings.wake_capture();
            sess.settings.fps.store(fps.clamp(5, 60), Ordering::Relaxed);
            let target_bitrate = if bitrate_kbps > 0 {
                bitrate_kbps
            } else {
                // Map legacy quality 25..95 -> 600..6000 kbps
                600 + ((quality.clamp(20, 95) as u32) * 55)
            };
            sess.settings
                .bitrate_kbps
                .store(target_bitrate.clamp(400, 12_000), Ordering::Relaxed);
            sess.settings.scale_x100.store(
                (scale.clamp(0.35, 1.0) * 100.0).round() as u8,
                Ordering::Relaxed,
            );
        }

        SessionMessage::RequestKeyframe => {
            sess.settings.force_keyframe.store(true, Ordering::Release);
            sess.settings.wake_capture();
        }

        SessionMessage::RtcOffer { sdp } => {
            sess.p2p.handle_offer(sdp);
        }

        SessionMessage::RtcIceCandidate {
            candidate,
            sdp_mid,
            sdp_mline_index,
        } => {
            sess.p2p.handle_ice_candidate(candidate, sdp_mid, sdp_mline_index);
        }

        SessionMessage::MouseMove { x, y } => {
            if !sess.permissions.allow_input {
                return;
            }
            let _ = input_tx.send(InputAction::MouseMove {
                x,
                y,
                bounds: sess.settings.bounds(),
            });
            if sess.settings.show_cursor.load(Ordering::Relaxed) {
                sess.settings.wake_capture();
            }
        }

        SessionMessage::MouseButton {
            button,
            pressed,
            x,
            y,
        } => {
            if !sess.permissions.allow_input {
                return;
            }
            let _ = input_tx.send(InputAction::MouseButton {
                button,
                pressed,
                x,
                y,
                bounds: sess.settings.bounds(),
            });
            sess.settings.wake_capture();
        }

        SessionMessage::MouseScroll { delta_x, delta_y } => {
            if !sess.permissions.allow_input {
                return;
            }
            let _ = input_tx.send(InputAction::MouseScroll { delta_x, delta_y });
            sess.settings.wake_capture();
        }

        SessionMessage::KeyEvent { code, key, pressed } => {
            if !sess.permissions.allow_input {
                return;
            }
            let _ = input_tx.send(InputAction::KeyEvent { code, key, pressed });
            sess.settings.wake_capture();
        }

        SessionMessage::ClipboardSync { text } => {
            if !sess.permissions.allow_clipboard {
                return;
            }
            if let Ok(mut guard) = sess.last_clipboard_text.lock() {
                *guard = text.clone();
            }
            set_clipboard_text(&text);
        }

        SessionMessage::FileListRequest { path } => {
            if !sess.permissions.allow_file_transfer {
                send_encrypted_session_msg(
                    out_tx,
                    session_id,
                    &sess.e2e,
                    &SessionMessage::FileListResponse {
                        current_path: String::new(),
                        parent_path: None,
                        entries: vec![],
                        error: Some(
                            "Permiso de transferencia de archivos deshabilitado por el anfitrión"
                                .into(),
                        ),
                    },
                );
                return;
            }
            let resp = list_directory(path);
            send_encrypted_session_msg(out_tx, session_id, &sess.e2e, &resp);
        }

        SessionMessage::FileDownloadRequest { path } => {
            if !sess.permissions.allow_file_transfer {
                return;
            }
            let sid = session_id.to_string();
            let e2e = sess.e2e.clone();
            let out_tx = out_tx.clone();
            tokio::task::spawn_blocking(move || match prepare_file_download(&path) {
                Ok(chunks) => {
                    for chunk in chunks {
                        send_encrypted_session_msg(&out_tx, &sid, &e2e, &chunk);
                    }
                }
                Err(err_msg) => {
                    send_encrypted_session_msg(
                        &out_tx,
                        &sid,
                        &e2e,
                        &SessionMessage::FileTransferStatus {
                            transfer_id: uuid::Uuid::new_v4().to_string(),
                            ok: false,
                            message: err_msg,
                        },
                    );
                }
            });
        }

        SessionMessage::FileChunk {
            transfer_id,
            file_name,
            target_dir,
            offset,
            data,
            is_last,
            ..
        } => {
            if !sess.permissions.allow_file_transfer {
                return;
            }
            if let Ok(mut mgr) = sess.upload_mgr.lock() {
                let (status, _) =
                    mgr.handle_chunk(transfer_id, file_name, target_dir, offset, data, is_last);
                if let Some(status) = status {
                    send_encrypted_session_msg(out_tx, session_id, &sess.e2e, &status);
                }
            }
        }

        SessionMessage::ClipboardFilePaste {
            transfer_id,
            file_name,
            offset,
            data,
            is_last,
            paste_after_receive,
            ..
        } => {
            if !sess.permissions.allow_file_transfer || !sess.permissions.allow_clipboard {
                return;
            }
            let (status, completed_path) = {
                let Ok(mut mgr) = sess.upload_mgr.lock() else {
                    return;
                };
                mgr.handle_chunk(transfer_id, file_name, None, offset, data, is_last)
            };

            if let Some(path) = completed_path {
                let path_str = path.to_string_lossy().to_string();
                if let Ok(mut guard) = sess.last_clipboard_files.lock() {
                    *guard = vec![path_str.clone()];
                }
                let placed = set_clipboard_files(&[path_str]);
                if placed && paste_after_receive && sess.permissions.allow_input {
                    let _ = input_tx.send(InputAction::TriggerPaste);
                }
            }

            if let Some(status) = status {
                send_encrypted_session_msg(out_tx, session_id, &sess.e2e, &status);
            }
        }

        _ => {}
    }
}

fn send_ws_signal(out_tx: &mpsc::UnboundedSender<WsMessage>, msg: &SignalMessage) {
    if let Ok(json) = serde_json::to_string(msg) {
        let _ = out_tx.send(WsMessage::Text(json.into()));
    }
}

pub(crate) fn send_encrypted_session_msg(
    out_tx: &mpsc::UnboundedSender<WsMessage>,
    session_id: &str,
    e2e: &E2eSession,
    msg: &SessionMessage,
) {
    if let Ok(bytes) = serde_json::to_vec(msg) {
        if let Ok(payload) = e2e.encrypt_b64(&bytes) {
            send_ws_signal(
                out_tx,
                &SignalMessage::EncryptedRelay {
                    session_id: session_id.to_string(),
                    payload,
                },
            );
        }
    }
}
