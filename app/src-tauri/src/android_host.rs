//! Native Android Host Agent (WebSocket signaling + E2E AES-256-GCM + WebRTC P2P).
//!
//! Bridges frames and input events with the Kotlin `ScreenCaptureService` (MediaProjection +
//! hardware MediaCodec H.264) and `RemoteControlAccessibilityService` over local loopback
//! TCP (`127.0.0.1:47821`).

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU16, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use remoteconnect_protocol::{
    DisplayInfo, E2eSession, EphemeralKeyPair, MouseBtn, SessionMessage, SessionPermissions,
    SignalMessage,
};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::{connect_async_with_config, tungstenite::Message as WsMessage};
use tracing::{info, warn};
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

const PACKET_VIDEO_H264: u8 = 0x04;
const P2P_CHUNK_SIZE: usize = 16 * 1024;
const P2P_MAX_BUFFERED_BYTES: usize = 128 * 1024;
const P2P_RESUME_BUFFERED_BYTES: usize = 64 * 1024;
const ANDROID_BRIDGE_ADDR: &str = "127.0.0.1:47821";

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub server_url: String,
    pub device_id: String,
    pub device_name: String,
    pub pin: String,
    pub pin_enabled: bool,
    pub custom_password: Option<String>,
    pub custom_password_enabled: bool,
    pub default_permissions: SessionPermissions,
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
        let p = provided.trim();
        if p.is_empty() {
            return false;
        }
        if self.pin_enabled && !self.pin.is_empty() && p == self.pin {
            return true;
        }
        if self.custom_password_enabled {
            if let Some(ref pw) = self.custom_password {
                if !pw.is_empty() && p == pw {
                    return true;
                }
            }
        }
        false
    }
}

#[derive(Debug, Clone, Serialize)]
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

pub enum AgentCommand {
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

/// Shared local bridge between Kotlin ScreenCaptureService/AccessibilityService and Rust P2P.
struct LocalAndroidBridge {
    /// Broadcast of ready-to-send H.264 media packets (`[0x04][1B kf][4B w][4B h][4B ts][h264]`)
    frame_tx: broadcast::Sender<Arc<Vec<u8>>>,
    /// Outbound newline-JSON commands to Kotlin (`mouse_button`, `mouse_move`, `request_keyframe`, etc.)
    kotlin_cmd_tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
}

impl LocalAndroidBridge {
    fn spawn() -> Arc<Self> {
        let (frame_tx, _) = broadcast::channel::<Arc<Vec<u8>>>(8);
        let bridge = Arc::new(Self {
            frame_tx,
            kotlin_cmd_tx: Mutex::new(None),
        });
        let bridge_srv = bridge.clone();
        tokio::spawn(async move {
            let Ok(listener) = TcpListener::bind(ANDROID_BRIDGE_ADDR).await else {
                warn!("Android host loopback bridge failed to bind {}", ANDROID_BRIDGE_ADDR);
                return;
            };
            info!("Android host loopback bridge listening on {}", ANDROID_BRIDGE_ADDR);
            while let Ok((stream, _)) = listener.accept().await {
                let _ = stream.set_nodelay(true);
                let (mut reader, mut writer) = stream.into_split();
                let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<String>();
                *bridge_srv.kotlin_cmd_tx.lock().await = Some(cmd_tx);

                let writer_task = tokio::spawn(async move {
                    while let Some(line) = cmd_rx.recv().await {
                        if writer.write_all(line.as_bytes()).await.is_err() {
                            break;
                        }
                        if writer.write_all(b"\n").await.is_err() {
                            break;
                        }
                    }
                });

                let frame_tx = bridge_srv.frame_tx.clone();
                let mut len_buf = [0u8; 4];
                loop {
                    if reader.read_exact(&mut len_buf).await.is_err() {
                        break;
                    }
                    let payload_len = u32::from_be_bytes(len_buf) as usize;
                    if payload_len == 0 || payload_len > 8 * 1024 * 1024 {
                        break;
                    }
                    let mut packet = vec![0u8; 1 + payload_len];
                    packet[0] = PACKET_VIDEO_H264;
                    if reader.read_exact(&mut packet[1..]).await.is_err() {
                        break;
                    }
                    let _ = frame_tx.send(Arc::new(packet));
                }

                writer_task.abort();
                *bridge_srv.kotlin_cmd_tx.lock().await = None;
            }
        });
        bridge
    }

    fn send_kotlin_json(&self, json: String) {
        if let Ok(guard) = self.kotlin_cmd_tx.try_lock() {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(json);
            }
        }
    }
}

struct AndroidP2p {
    session_id: String,
    e2e: Arc<E2eSession>,
    ws_out_tx: mpsc::UnboundedSender<WsMessage>,
    pc: Arc<Mutex<Option<Arc<RTCPeerConnection>>>>,
    pending_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>>,
    remote_desc_set: Arc<AtomicBool>,
    media_ready: Arc<AtomicBool>,
    media_dc_slot: Arc<Mutex<Option<Arc<RTCDataChannel>>>>,
    bridge: Arc<LocalAndroidBridge>,
    on_control_msg: Arc<dyn Fn(SessionMessage) + Send + Sync>,
}

impl AndroidP2p {
    fn spawn<F>(
        session_id: String,
        e2e: Arc<E2eSession>,
        ws_out_tx: mpsc::UnboundedSender<WsMessage>,
        bridge: Arc<LocalAndroidBridge>,
        on_control_msg: F,
    ) -> Arc<Self>
    where
        F: Fn(SessionMessage) + Send + Sync + 'static,
    {
        let media_ready = Arc::new(AtomicBool::new(false));
        let media_dc_slot: Arc<Mutex<Option<Arc<RTCDataChannel>>>> = Arc::new(Mutex::new(None));
        let next_msg_id = Arc::new(AtomicU16::new(1));

        // Pump H.264 frames from Kotlin ScreenCaptureService -> WebRTC P2P media DataChannel
        {
            let mut frame_rx = bridge.frame_tx.subscribe();
            let dc_slot = media_dc_slot.clone();
            let ready = media_ready.clone();
            let msg_id_gen = next_msg_id.clone();
            tokio::spawn(async move {
                loop {
                    match frame_rx.recv().await {
                        Ok(raw_packet) => {
                            if !ready.load(Ordering::Acquire) {
                                continue;
                            }
                            let Some(dc) = dc_slot.lock().await.clone() else {
                                continue;
                            };
                            let mut buffered = dc.buffered_amount().await;
                            if buffered > P2P_MAX_BUFFERED_BYTES {
                                let is_kf = raw_packet.get(1).copied() == Some(1);
                                if !is_kf {
                                    continue;
                                }
                                let wait_start = Instant::now();
                                while buffered > P2P_RESUME_BUFFERED_BYTES
                                    && ready.load(Ordering::Relaxed)
                                    && wait_start.elapsed() < Duration::from_millis(200)
                                {
                                    tokio::time::sleep(Duration::from_millis(5)).await;
                                    buffered = dc.buffered_amount().await;
                                }
                            }
                            let msg_id = msg_id_gen.fetch_add(1, Ordering::Relaxed);
                            let total_chunks =
                                ((raw_packet.len() + P2P_CHUNK_SIZE - 1) / P2P_CHUNK_SIZE).max(1);
                            if total_chunks > u16::MAX as usize {
                                continue;
                            }
                            let total_u16 = total_chunks as u16;
                            for (idx, chunk) in raw_packet.chunks(P2P_CHUNK_SIZE).enumerate() {
                                let mut framed = Vec::with_capacity(6 + chunk.len());
                                framed.extend_from_slice(&msg_id.to_be_bytes());
                                framed.extend_from_slice(&(idx as u16).to_be_bytes());
                                framed.extend_from_slice(&total_u16.to_be_bytes());
                                framed.extend_from_slice(chunk);
                                if dc.send(&Bytes::from(framed)).await.is_err() {
                                    ready.store(false, Ordering::Release);
                                    *dc_slot.lock().await = None;
                                    break;
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        Arc::new(Self {
            session_id,
            e2e,
            ws_out_tx,
            pc: Arc::new(Mutex::new(None)),
            pending_candidates: Arc::new(Mutex::new(Vec::new())),
            remote_desc_set: Arc::new(AtomicBool::new(false)),
            media_ready,
            media_dc_slot,
            bridge,
            on_control_msg: Arc::new(on_control_msg),
        })
    }

    fn handle_offer(self: &Arc<Self>, sdp: String) {
        let mgr = self.clone();
        tokio::spawn(async move {
            if let Err(e) = mgr.handle_offer_inner(sdp).await {
                warn!("Android P2P offer failed: {}", e);
            }
        });
    }

    async fn handle_offer_inner(self: &Arc<Self>, sdp: String) -> Result<(), String> {
        if let Some(old) = self.pc.lock().await.take() {
            let _ = old.close().await;
        }
        self.media_ready.store(false, Ordering::Release);
        self.remote_desc_set.store(false, Ordering::Release);

        let api = APIBuilder::new().build();
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec![
                    "stun:stun.cloudflare.com:3478".into(),
                    "stun:stun.l.google.com:19302".into(),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let pc = Arc::new(
            api.new_peer_connection(config)
                .await
                .map_err(|e| e.to_string())?,
        );

        {
            let sid = self.session_id.clone();
            let e2e = self.e2e.clone();
            let ws_tx = self.ws_out_tx.clone();
            pc.on_ice_candidate(Box::new(move |cand_opt| {
                let sid = sid.clone();
                let e2e = e2e.clone();
                let ws_tx = ws_tx.clone();
                Box::pin(async move {
                    if let Some(cand) = cand_opt {
                        if let Ok(init) = cand.to_json() {
                            if !init.candidate.is_empty() {
                                send_encrypted_session_msg(
                                    &ws_tx,
                                    &sid,
                                    &e2e,
                                    &SessionMessage::RtcIceCandidate {
                                        candidate: init.candidate,
                                        sdp_mid: init.sdp_mid,
                                        sdp_mline_index: init.sdp_mline_index,
                                    },
                                );
                            }
                        }
                    }
                })
            }));
        }

        {
            let media_ready = self.media_ready.clone();
            let media_slot = self.media_dc_slot.clone();
            pc.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
                let media_ready = media_ready.clone();
                let media_slot = media_slot.clone();
                Box::pin(async move {
                    if matches!(
                        state,
                        RTCPeerConnectionState::Disconnected
                            | RTCPeerConnectionState::Failed
                            | RTCPeerConnectionState::Closed
                    ) {
                        media_ready.store(false, Ordering::Release);
                        *media_slot.lock().await = None;
                    }
                })
            }));
        }

        {
            let media_ready = self.media_ready.clone();
            let media_slot = self.media_dc_slot.clone();
            let bridge = self.bridge.clone();
            let on_control = self.on_control_msg.clone();

            pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
                let label = dc.label().to_string();
                let media_ready = media_ready.clone();
                let media_slot = media_slot.clone();
                let bridge = bridge.clone();
                let on_control = on_control.clone();
                Box::pin(async move {
                    if label == "media" {
                        let dc_open = dc.clone();
                        let slot_open = media_slot.clone();
                        let ready_open = media_ready.clone();
                        let br_open = bridge.clone();
                        dc.on_open(Box::new(move || {
                            let dc_open = dc_open.clone();
                            let slot_open = slot_open.clone();
                            let ready_open = ready_open.clone();
                            let br_open = br_open.clone();
                            Box::pin(async move {
                                *slot_open.lock().await = Some(dc_open);
                                br_open.send_kotlin_json(r#"{"cmd":"request_keyframe"}"#.into());
                                ready_open.store(true, Ordering::Release);
                            })
                        }));
                        if dc.ready_state() == RTCDataChannelState::Open {
                            *media_slot.lock().await = Some(dc.clone());
                            bridge.send_kotlin_json(r#"{"cmd":"request_keyframe"}"#.into());
                            media_ready.store(true, Ordering::Release);
                        }
                    } else if label == "control" {
                        let dc_reply = dc.clone();
                        dc.on_message(Box::new(move |msg: DataChannelMessage| {
                            let on_control = on_control.clone();
                            let dc_reply = dc_reply.clone();
                            Box::pin(async move {
                                if let Ok(smsg) =
                                    serde_json::from_slice::<SessionMessage>(&msg.data)
                                {
                                    if let SessionMessage::P2pPing { timestamp } = smsg {
                                        if let Ok(pong) = serde_json::to_string(
                                            &SessionMessage::P2pPong { timestamp },
                                        ) {
                                            let _ = dc_reply.send_text(pong).await;
                                        }
                                        return;
                                    }
                                    on_control(smsg);
                                }
                            })
                        }));
                    }
                })
            }));
        }

        let offer = RTCSessionDescription::offer(sdp).map_err(|e| e.to_string())?;
        pc.set_remote_description(offer)
            .await
            .map_err(|e| e.to_string())?;
        self.remote_desc_set.store(true, Ordering::Release);

        let queued: Vec<RTCIceCandidateInit> = {
            let mut q = self.pending_candidates.lock().await;
            std::mem::take(&mut *q)
        };
        for cand in queued {
            let _ = pc.add_ice_candidate(cand).await;
        }

        let answer = pc.create_answer(None).await.map_err(|e| e.to_string())?;
        let answer_sdp = answer.sdp.clone();
        pc.set_local_description(answer)
            .await
            .map_err(|e| e.to_string())?;
        *self.pc.lock().await = Some(pc);

        send_encrypted_session_msg(
            &self.ws_out_tx,
            &self.session_id,
            &self.e2e,
            &SessionMessage::RtcAnswer { sdp: answer_sdp },
        );
        Ok(())
    }

    fn handle_ice_candidate(
        self: &Arc<Self>,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    ) {
        let mgr = self.clone();
        tokio::spawn(async move {
            let init = RTCIceCandidateInit {
                candidate,
                sdp_mid,
                sdp_mline_index,
                ..Default::default()
            };
            if mgr.remote_desc_set.load(Ordering::Acquire) {
                if let Some(pc) = mgr.pc.lock().await.clone() {
                    let _ = pc.add_ice_candidate(init).await;
                }
            } else {
                mgr.pending_candidates.lock().await.push(init);
            }
        });
    }

    fn close(&self) {
        self.media_ready.store(false, Ordering::Release);
        let pc_slot = self.pc.clone();
        let media_slot = self.media_dc_slot.clone();
        tokio::spawn(async move {
            *media_slot.lock().await = None;
            if let Some(pc) = pc_slot.lock().await.take() {
                let _ = pc.close().await;
            }
        });
    }
}

struct RunningSession {
    e2e: Arc<E2eSession>,
    permissions: SessionPermissions,
    p2p: Arc<AndroidP2p>,
}

struct PendingApproval {
    viewer_name: String,
    viewer_public_key: String,
}

fn send_ws_signal(tx: &mpsc::UnboundedSender<WsMessage>, msg: &SignalMessage) {
    if let Ok(json) = serde_json::to_string(msg) {
        let _ = tx.send(WsMessage::Text(json.into()));
    }
}

fn send_encrypted_session_msg(
    tx: &mpsc::UnboundedSender<WsMessage>,
    session_id: &str,
    e2e: &E2eSession,
    msg: &SessionMessage,
) {
    if let Ok(plain) = serde_json::to_vec(msg) {
        if let Ok(payload) = e2e.encrypt_b64(&plain) {
            send_ws_signal(
                tx,
                &SignalMessage::EncryptedRelay {
                    session_id: session_id.to_string(),
                    payload,
                },
            );
        }
    }
}

pub fn start_android_host(
    config: AgentConfig,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
) -> AgentHandle {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let bridge = LocalAndroidBridge::spawn();
    tokio::spawn(run_loop(config, event_tx, cmd_rx, bridge));
    AgentHandle { cmd_tx }
}

async fn run_loop(
    mut config: AgentConfig,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<AgentCommand>,
    bridge: Arc<LocalAndroidBridge>,
) {
    loop {
        let _ = event_tx.send(AgentEvent::Connecting {
            server_url: config.server_url.clone(),
        });

        let connect_res = tokio::time::timeout(
            Duration::from_secs(12),
            connect_async_with_config(&config.server_url, None, true),
        )
        .await;

        match connect_res {
            Ok(Ok((ws_stream, _))) => {
                let should_stop =
                    handle_ws(ws_stream, &mut config, &event_tx, &mut cmd_rx, &bridge).await;
                if should_stop {
                    return;
                }
                let _ = event_tx.send(AgentEvent::Disconnected {
                    reason: "Reconectando con el servidor...".into(),
                });
            }
            Ok(Err(e)) => {
                let _ = event_tx.send(AgentEvent::Disconnected {
                    reason: format!("Conexión fallida: {}", e),
                });
            }
            Err(_) => {
                let _ = event_tx.send(AgentEvent::Disconnected {
                    reason: "Timeout conectando al servidor".into(),
                });
            }
        }

        let sleep = tokio::time::sleep(Duration::from_secs(3));
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

async fn handle_ws(
    ws_stream: WsStream,
    config: &mut AgentConfig,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    cmd_rx: &mut mpsc::UnboundedReceiver<AgentCommand>,
    bridge: &Arc<LocalAndroidBridge>,
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

    send_ws_signal(
        &out_tx,
        &SignalMessage::RegisterHost {
            device_id: config.device_id.clone(),
            device_name: config.device_name.clone(),
            os: "android".into(),
            auth_token: config.auth_token.clone(),
            unattended_enabled: config.unattended_enabled(),
        },
    );

    let mut sessions: HashMap<String, RunningSession> = HashMap::new();
    let mut pending: HashMap<String, PendingApproval> = HashMap::new();
    let (p2p_in_tx, mut p2p_in_rx) = mpsc::unbounded_channel::<(String, SessionMessage)>();

    let mut ping_interval = tokio::time::interval(Duration::from_secs(15));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ping_ticks: u64 = 0;
    let mut last_activity = Instant::now();
    let mut stop_requested = false;

    loop {
        tokio::select! {
            Some((sid, smsg)) = p2p_in_rx.recv() => {
                if let Some(sess) = sessions.get(&sid) {
                    handle_session_msg(smsg, sess, bridge);
                }
            }

            _ = ping_interval.tick() => {
                if writer.is_finished() || last_activity.elapsed() > Duration::from_secs(40) {
                    break;
                }
                ping_ticks += 1;
                let ts = chrono::Utc::now().timestamp_millis() as u64;
                send_ws_signal(&out_tx, &SignalMessage::Ping { timestamp: ts });
                let _ = out_tx.send(WsMessage::Ping(Vec::new().into()));
                if ping_ticks % 3 == 0 {
                    send_ws_signal(&out_tx, &SignalMessage::RegisterHost {
                        device_id: config.device_id.clone(),
                        device_name: config.device_name.clone(),
                        os: "android".into(),
                        auth_token: config.auth_token.clone(),
                        unattended_enabled: config.unattended_enabled(),
                    });
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
                        if let Some(s) = sessions.remove(&session_id) {
                            s.p2p.close();
                        }
                        send_ws_signal(&out_tx, &SignalMessage::Disconnect {
                            session_id: session_id.clone(),
                            reason: Some("Host Android cerró la sesión".into()),
                        });
                        let _ = event_tx.send(AgentEvent::SessionEnded {
                            session_id,
                            reason: "Cerrado por anfitrión".into(),
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
                                    &p2p_in_tx,
                                    bridge,
                                    event_tx,
                                    &mut sessions,
                                );
                            } else {
                                send_ws_signal(&out_tx, &SignalMessage::ConnectResponse {
                                    session_id,
                                    accepted: false,
                                    reason: Some("Conexión rechazada".into()),
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
                let Some(Ok(ws_msg)) = incoming else {
                    break;
                };
                last_activity = Instant::now();
                match ws_msg {
                    WsMessage::Ping(p) => { let _ = out_tx.send(WsMessage::Pong(p)); }
                    WsMessage::Pong(_) => {}
                    WsMessage::Close(_) => break,
                    WsMessage::Text(text) => {
                        let Ok(signal) = serde_json::from_str::<SignalMessage>(&text) else {
                            continue;
                        };
                        match signal {
                            SignalMessage::Registered { device_id, server_version } => {
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
                                if cred_ok {
                                    start_session(
                                        session_id,
                                        viewer_name,
                                        &viewer_public_key,
                                        config.default_permissions,
                                        &out_tx,
                                        &p2p_in_tx,
                                        bridge,
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
                                            handle_session_msg(smsg, sess, bridge);
                                        }
                                    }
                                }
                            }
                            SignalMessage::Disconnect { session_id, reason } => {
                                if let Some(s) = sessions.remove(&session_id) {
                                    s.p2p.close();
                                }
                                pending.remove(&session_id);
                                let _ = event_tx.send(AgentEvent::SessionEnded {
                                    session_id,
                                    reason: reason.unwrap_or_else(|| "Desconectado".into()),
                                });
                            }
                            SignalMessage::Error { message, .. } => {
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

    for (_, sess) in sessions.drain() {
        sess.p2p.close();
    }
    writer.abort();
    stop_requested
}

#[allow(clippy::too_many_arguments)]
fn start_session(
    session_id: String,
    viewer_name: String,
    viewer_public_key: &str,
    permissions: SessionPermissions,
    out_tx: &mpsc::UnboundedSender<WsMessage>,
    p2p_in_tx: &mpsc::UnboundedSender<(String, SessionMessage)>,
    bridge: &Arc<LocalAndroidBridge>,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    sessions: &mut HashMap<String, RunningSession>,
) {
    let keypair = EphemeralKeyPair::generate();
    let host_pub = keypair.public_key_b64();
    let Ok(e2e) = keypair.derive_session(viewer_public_key, &session_id).map(Arc::new) else {
        return;
    };

    let displays = vec![DisplayInfo {
        id: 0,
        name: "Pantalla Android".into(),
        width: 1080,
        height: 2400,
        is_primary: true,
    }];

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

    let sid_ctrl = session_id.clone();
    let ctrl_tx = p2p_in_tx.clone();
    let p2p = AndroidP2p::spawn(
        session_id.clone(),
        e2e.clone(),
        out_tx.clone(),
        bridge.clone(),
        move |smsg| {
            let _ = ctrl_tx.send((sid_ctrl.clone(), smsg));
        },
    );

    sessions.insert(
        session_id.clone(),
        RunningSession {
            e2e,
            permissions,
            p2p,
        },
    );
    bridge.send_kotlin_json(r#"{"cmd":"request_keyframe"}"#.into());
    let _ = event_tx.send(AgentEvent::SessionStarted {
        session_id,
        viewer_name,
    });
}

fn handle_session_msg(
    msg: SessionMessage,
    sess: &RunningSession,
    bridge: &Arc<LocalAndroidBridge>,
) {
    match msg {
        SessionMessage::RtcOffer { sdp } => sess.p2p.handle_offer(sdp),
        SessionMessage::RtcIceCandidate {
            candidate,
            sdp_mid,
            sdp_mline_index,
        } => sess
            .p2p
            .handle_ice_candidate(candidate, sdp_mid, sdp_mline_index),
        SessionMessage::RequestKeyframe => {
            bridge.send_kotlin_json(r#"{"cmd":"request_keyframe"}"#.into());
        }
        SessionMessage::MouseMove { x, y } if sess.permissions.allow_input => {
            bridge.send_kotlin_json(format!(r#"{{"cmd":"mouse_move","x":{:.5},"y":{:.5}}}"#, x, y));
        }
        SessionMessage::MouseButton {
            button,
            pressed,
            x,
            y,
        } if sess.permissions.allow_input => {
            let btn = match button {
                MouseBtn::Left => "left",
                MouseBtn::Right => "right",
                MouseBtn::Middle => "middle",
            };
            bridge.send_kotlin_json(format!(
                r#"{{"cmd":"mouse_button","button":"{}","pressed":{},"x":{:.5},"y":{:.5}}}"#,
                btn, pressed, x, y
            ));
        }
        SessionMessage::MouseScroll { delta_x, delta_y } if sess.permissions.allow_input => {
            bridge.send_kotlin_json(format!(
                r#"{{"cmd":"mouse_scroll","dx":{},"dy":{}}}"#,
                delta_x, delta_y
            ));
        }
        SessionMessage::KeyEvent { code, key, pressed } if sess.permissions.allow_input => {
            let payload = serde_json::json!({
                "cmd": "key_event",
                "code": code,
                "key": key,
                "pressed": pressed
            });
            bridge.send_kotlin_json(payload.to_string());
        }
        _ => {}
    }
}
