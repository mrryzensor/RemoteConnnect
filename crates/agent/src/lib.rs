pub mod audio;
pub mod capture;
pub mod clipboard;
pub mod files;
pub mod input;

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use remoteconnect_protocol::{
    E2eSession, EphemeralKeyPair, SessionMessage, SessionPermissions, SignalMessage,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{error, info, warn};

use crate::audio::spawn_audio_capture;
use crate::capture::{capture_frame_jpeg, get_display_bounds, list_displays};
use crate::clipboard::{
    get_clipboard_files, get_clipboard_text, set_clipboard_files, set_clipboard_text,
};
use crate::files::{list_directory, prepare_file_download, UploadManager};
use crate::input::InputController;

pub const PACKET_VIDEO_JPEG: u8 = 0x01;
pub const PACKET_AUDIO_PCM16: u8 = 0x02;

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

struct StreamSettings {
    fps: AtomicU8,
    quality: AtomicU8,
    display_index: AtomicUsize,
    scale_x100: AtomicU8,
    active: Arc<AtomicBool>,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            fps: AtomicU8::new(20),
            quality: AtomicU8::new(65),
            display_index: AtomicUsize::new(0),
            scale_x100: AtomicU8::new(85),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

struct RunningSession {
    e2e: Arc<E2eSession>,
    permissions: SessionPermissions,
    settings: Arc<StreamSettings>,
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

        match connect_async(&config.server_url).await {
            Ok((ws_stream, _)) => {
                info!("Connected to server {}", config.server_url);
                let should_stop =
                    handle_connection(ws_stream, &mut config, &event_tx, &mut cmd_rx).await;
                if should_stop {
                    info!("Agent stopped by command");
                    return;
                }
            }
            Err(e) => {
                let msg = format!("Connection to {} failed: {}", config.server_url, e);
                warn!("{}", msg);
                let _ = event_tx.send(AgentEvent::Disconnected { reason: msg });
            }
        }

        let sleep = tokio::time::sleep(Duration::from_secs(4));
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
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<WsMessage>();

    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
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
    let input_ctrl = Arc::new(Mutex::new(InputController::new().ok()));

    let mut ping_interval = tokio::time::interval(Duration::from_secs(20));
    let mut stop_requested = false;

    loop {
        tokio::select! {
            _ = ping_interval.tick() => {
                let ts = chrono::Utc::now().timestamp_millis() as u64;
                send_ws_signal(&out_tx, &SignalMessage::Ping { timestamp: ts });
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

                if let WsMessage::Text(text) = ws_msg {
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
                                            &input_ctrl,
                                            &out_tx,
                                        );
                                    }
                                }
                            }
                        }

                        SignalMessage::Disconnect { session_id, reason } => {
                            if let Some(mut s) = sessions.remove(&session_id) {
                                s.settings.active.store(false, Ordering::Relaxed);
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
                } else if let WsMessage::Close(_) = ws_msg {
                    break;
                }
            }
        }
    }

    for (sid, mut sess) in sessions.drain() {
        sess.settings.active.store(false, Ordering::Relaxed);
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

fn start_session(
    session_id: String,
    viewer_name: String,
    viewer_public_key: &str,
    permissions: SessionPermissions,
    out_tx: &mpsc::UnboundedSender<WsMessage>,
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

    let settings = Arc::new(StreamSettings::default());
    let last_clipboard_text = Arc::new(Mutex::new(get_clipboard_text().unwrap_or_default()));
    let initial_files: Vec<String> = get_clipboard_files()
        .into_iter()
        .map(|f| f.path)
        .collect();
    let last_clipboard_files = Arc::new(Mutex::new(initial_files));
    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

    // Spawn remote audio capture & streaming task if permitted
    if permissions.allow_audio {
        let sid = session_id.clone();
        let e2e = e2e.clone();
        let active = settings.active.clone();
        let out_tx = out_tx.clone();
        let mut audio_rx = spawn_audio_capture(active.clone());

        tokio::spawn(async move {
            while let Some(chunk) = audio_rx.recv().await {
                if !active.load(Ordering::Relaxed) {
                    break;
                }
                // Plaintext: [1B type=0x02][4B BE sample_rate][2B BE channels][i16 LE PCM...]
                let mut plain = Vec::with_capacity(7 + chunk.pcm_i16_le.len());
                plain.push(PACKET_AUDIO_PCM16);
                plain.extend_from_slice(&chunk.sample_rate.to_be_bytes());
                plain.extend_from_slice(&chunk.channels.to_be_bytes());
                plain.extend_from_slice(&chunk.pcm_i16_le);

                if let Ok(encrypted) = e2e.encrypt_bytes(&plain) {
                    let mut packet = Vec::with_capacity(36 + encrypted.len());
                    packet.extend_from_slice(sid.as_bytes());
                    packet.extend_from_slice(&encrypted);
                    if out_tx.send(WsMessage::Binary(packet.into())).is_err() {
                        break;
                    }
                }
            }
        });
    }

    // Spawn screen capture & clipboard polling task
    {
        let sid = session_id.clone();
        let e2e = e2e.clone();
        let settings = settings.clone();
        let out_tx = out_tx.clone();
        let last_cb_text = last_clipboard_text.clone();
        let last_cb_files = last_clipboard_files.clone();

        tokio::spawn(async move {
            let mut last_cb_check = Instant::now();
            loop {
                if !settings.active.load(Ordering::Relaxed) || stop_rx.try_recv().is_ok() {
                    break;
                }

                let start = Instant::now();
                let fps = settings.fps.load(Ordering::Relaxed).clamp(1, 30);
                let quality = settings.quality.load(Ordering::Relaxed);
                let display_idx = settings.display_index.load(Ordering::Relaxed);
                let scale = (settings.scale_x100.load(Ordering::Relaxed) as f32) / 100.0;

                let cap_res = tokio::task::spawn_blocking(move || {
                    capture_frame_jpeg(display_idx, scale, quality)
                })
                .await;

                if let Ok(Ok((w, h, jpeg))) = cap_res {
                    // Plaintext: [1B type=0x01][4B BE width][4B BE height][JPEG bytes]
                    let mut plain = Vec::with_capacity(9 + jpeg.len());
                    plain.push(PACKET_VIDEO_JPEG);
                    plain.extend_from_slice(&w.to_be_bytes());
                    plain.extend_from_slice(&h.to_be_bytes());
                    plain.extend_from_slice(&jpeg);

                    if let Ok(encrypted) = e2e.encrypt_bytes(&plain) {
                        let mut packet = Vec::with_capacity(36 + encrypted.len());
                        packet.extend_from_slice(sid.as_bytes());
                        packet.extend_from_slice(&encrypted);
                        if out_tx.send(WsMessage::Binary(packet.into())).is_err() {
                            break;
                        }
                    }
                }

                // Poll host clipboard (text & CF_HDROP files) every 1.0s
                if permissions.allow_clipboard && last_cb_check.elapsed() >= Duration::from_millis(1000) {
                    last_cb_check = Instant::now();

                    // 1. Check copied files (Ctrl+C in Explorer)
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

                    // 2. Check copied text
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

                let frame_budget = Duration::from_millis(1000 / (fps as u64));
                let elapsed = start.elapsed();
                if elapsed < frame_budget {
                    tokio::time::sleep(frame_budget - elapsed).await;
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
    input_ctrl: &Arc<Mutex<Option<InputController>>>,
    out_tx: &mpsc::UnboundedSender<WsMessage>,
) {
    match msg {
        SessionMessage::StreamConfig {
            fps,
            quality,
            display_index,
            scale,
        } => {
            sess.settings.fps.store(fps.clamp(1, 30), Ordering::Relaxed);
            sess.settings
                .quality
                .store(quality.clamp(20, 95), Ordering::Relaxed);
            sess.settings
                .display_index
                .store(display_index, Ordering::Relaxed);
            sess.settings.scale_x100.store(
                (scale.clamp(0.3, 1.0) * 100.0).round() as u8,
                Ordering::Relaxed,
            );
        }

        SessionMessage::MouseMove { x, y } => {
            if !sess.permissions.allow_input {
                return;
            }
            let disp = sess.settings.display_index.load(Ordering::Relaxed);
            if let Ok(bounds) = get_display_bounds(disp) {
                if let Ok(mut guard) = input_ctrl.lock() {
                    if let Some(ctrl) = guard.as_mut() {
                        ctrl.move_mouse(x, y, bounds);
                    }
                }
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
            let disp = sess.settings.display_index.load(Ordering::Relaxed);
            if let Ok(bounds) = get_display_bounds(disp) {
                if let Ok(mut guard) = input_ctrl.lock() {
                    if let Some(ctrl) = guard.as_mut() {
                        ctrl.mouse_button(button, pressed, x, y, bounds);
                    }
                }
            }
        }

        SessionMessage::MouseScroll { delta_x, delta_y } => {
            if !sess.permissions.allow_input {
                return;
            }
            if let Ok(mut guard) = input_ctrl.lock() {
                if let Some(ctrl) = guard.as_mut() {
                    ctrl.mouse_scroll(delta_x, delta_y);
                }
            }
        }

        SessionMessage::KeyEvent { code, key, pressed } => {
            if !sess.permissions.allow_input {
                return;
            }
            if let Ok(mut guard) = input_ctrl.lock() {
                if let Some(ctrl) = guard.as_mut() {
                    ctrl.key_event(&code, &key, pressed);
                }
            }
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
                        error: Some("Permiso de transferencia de archivos deshabilitado por el anfitrión".into()),
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
                let placed = set_clipboard_files(&[path_str.clone()]);
                if placed && paste_after_receive && sess.permissions.allow_input {
                    if let Ok(mut guard) = input_ctrl.lock() {
                        if let Some(ctrl) = guard.as_mut() {
                            ctrl.trigger_paste();
                        }
                    }
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

fn send_encrypted_session_msg(
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
