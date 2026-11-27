use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use remoteconnect_protocol::{normalize_device_id, SignalMessage};
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::state::{ActiveSession, HostPeer, OutboundTx, PeerChannels, ServerState};

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_secs: u64,
    pub online_hosts: usize,
    pub active_sessions: usize,
    pub auth_required: bool,
}

pub async fn health(State(state): State<Arc<ServerState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.started_at.elapsed().as_secs(),
        online_hosts: state.hosts.len(),
        active_sessions: state.sessions.len(),
        auth_required: state.auth_token.is_some(),
    })
}

#[derive(Serialize)]
pub struct DeviceStatusResponse {
    pub device_id: String,
    pub online: bool,
    pub device_name: Option<String>,
    pub os: Option<String>,
    pub unattended_enabled: Option<bool>,
}

pub async fn device_status(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Json<DeviceStatusResponse> {
    let normalized = normalize_device_id(&id);
    if let Some(host) = state.hosts.get(&normalized) {
        Json(DeviceStatusResponse {
            device_id: normalized,
            online: true,
            device_name: Some(host.device_name.clone()),
            os: Some(host.os.clone()),
            unattended_enabled: Some(host.unattended_enabled),
        })
    } else {
        Json(DeviceStatusResponse {
            device_id: normalized,
            online: false,
            device_name: None,
            os: None,
            unattended_enabled: None,
        })
    }
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
    ws.max_message_size(16 * 1024 * 1024)
        .on_upgrade(move |socket| handle_socket(socket, state))
}

#[derive(Debug, Clone)]
enum PeerRole {
    Unidentified,
    Host { device_id: String },
    Viewer { session_id: String },
}

fn send_signal(tx: &OutboundTx, msg: &SignalMessage) {
    if let Ok(json) = serde_json::to_string(msg) {
        let _ = tx.send(Message::Text(json.into()));
    }
}

async fn handle_socket(socket: WebSocket, state: Arc<ServerState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    // High-priority control/input/signaling channel
    let (control_tx, mut control_rx) = mpsc::unbounded_channel::<Message>();
    // Lossless media relay channel (never drops mid-GOP H.264 delta frames, avoiding macroblock corruption;
    // backpressure is handled at the host agent before encoding)
    let (media_tx, mut media_rx) = mpsc::unbounded_channel::<Message>();
    let peer_ch = PeerChannels {
        control_tx: control_tx.clone(),
        media_tx,
    };

    let write_task = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                biased;
                m = control_rx.recv() => match m {
                    Some(m) => m,
                    None => break,
                },
                v = media_rx.recv() => match v {
                    Some(m) => m,
                    None => break,
                },
            };
            if ws_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut role = PeerRole::Unidentified;

    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Text(text) => {
                let parsed: SignalMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("Invalid JSON signal: {}", e);
                        send_signal(
                            &control_tx,
                            &SignalMessage::Error {
                                code: "invalid_message".into(),
                                message: format!("Malformed signal: {}", e),
                            },
                        );
                        continue;
                    }
                };

                handle_signal_message(parsed, &mut role, &peer_ch, &state);
            }
            Message::Binary(bin) => {
                // Fast-path E2E encrypted binary frame relay:
                // First 36 bytes = UTF-8 UUID session_id, rest = opaque E2E encrypted payload.
                if bin.len() > 36 {
                    if let Ok(session_id) = std::str::from_utf8(&bin[..36]) {
                        if let Some(sess) = state.sessions.get(session_id) {
                            if sess.established {
                                match &role {
                                    PeerRole::Host { .. } => {
                                        let _ = sess.viewer_tx.media_tx.send(Message::Binary(bin));
                                    }
                                    PeerRole::Viewer { .. } => {
                                        let _ = sess.host_tx.media_tx.send(Message::Binary(bin));
                                    }
                                    PeerRole::Unidentified => {}
                                }
                            }
                        }
                    }
                }
            }
            Message::Ping(payload) => {
                let _ = control_tx.send(Message::Pong(payload));
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    // Cleanup on disconnect
    cleanup_peer(&role, &state);
    write_task.abort();
}

fn handle_signal_message(
    msg: SignalMessage,
    role: &mut PeerRole,
    peer_ch: &PeerChannels,
    state: &Arc<ServerState>,
) {
    let tx = &peer_ch.control_tx;
    match msg {
        SignalMessage::RegisterHost {
            device_id,
            device_name,
            os,
            auth_token,
            unattended_enabled,
        } => {
            if !state.verify_token(&auth_token) {
                send_signal(
                    tx,
                    &SignalMessage::Error {
                        code: "unauthorized".into(),
                        message: "Invalid server auth token".into(),
                    },
                );
                return;
            }
            let normalized = normalize_device_id(&device_id);
            info!(
                device_id = %normalized,
                device_name = %device_name,
                os = %os,
                "Host registered"
            );
            state.hosts.insert(
                normalized.clone(),
                HostPeer {
                    device_id: normalized.clone(),
                    device_name,
                    os,
                    unattended_enabled,
                    connected_at: Utc::now(),
                    tx: peer_ch.clone(),
                },
            );
            *role = PeerRole::Host {
                device_id: normalized.clone(),
            };
            send_signal(
                tx,
                &SignalMessage::Registered {
                    device_id: normalized,
                    server_version: env!("CARGO_PKG_VERSION").into(),
                },
            );
        }

        SignalMessage::ConnectRequest {
            target_device_id,
            viewer_name,
            pin,
            public_key,
            auth_token,
        } => {
            if !state.verify_token(&auth_token) {
                send_signal(
                    tx,
                    &SignalMessage::Error {
                        code: "unauthorized".into(),
                        message: "Invalid server auth token".into(),
                    },
                );
                return;
            }

            let target = normalize_device_id(&target_device_id);
            let Some(host) = state.hosts.get(&target) else {
                send_signal(
                    tx,
                    &SignalMessage::Error {
                        code: "host_offline".into(),
                        message: format!("Device {} is not online", target),
                    },
                );
                return;
            };

            let session_id = Uuid::new_v4().to_string();
            let host_tx = host.tx.clone();
            drop(host);

            state.sessions.insert(
                session_id.clone(),
                ActiveSession {
                    session_id: session_id.clone(),
                    host_device_id: target.clone(),
                    host_tx: host_tx.clone(),
                    viewer_tx: peer_ch.clone(),
                    established: false,
                    created_at: Utc::now(),
                },
            );

            *role = PeerRole::Viewer {
                session_id: session_id.clone(),
            };

            info!(
                session_id = %session_id,
                target = %target,
                viewer = %viewer_name,
                "Forwarding connection request to host"
            );

            send_signal(
                &host_tx.control_tx,
                &SignalMessage::IncomingConnection {
                    session_id,
                    viewer_name,
                    pin,
                    viewer_public_key: public_key,
                },
            );
        }

        SignalMessage::ConnectResponse {
            session_id,
            accepted,
            reason,
            host_public_key,
            displays,
            permissions,
        } => {
            let mut host_name = None;
            if let Some(mut sess) = state.sessions.get_mut(&session_id) {
                if let Some(host) = state.hosts.get(&sess.host_device_id) {
                    host_name = Some(host.device_name.clone());
                }
                sess.established = accepted;
                let viewer_tx = sess.viewer_tx.control_tx.clone();
                drop(sess);

                if !accepted {
                    state.sessions.remove(&session_id);
                }

                info!(session_id = %session_id, accepted = accepted, "Host responded to session");
                send_signal(
                    &viewer_tx,
                    &SignalMessage::SessionReady {
                        session_id,
                        accepted,
                        reason,
                        host_public_key,
                        host_name,
                        displays,
                        permissions,
                    },
                );
            }
        }

        SignalMessage::EncryptedRelay {
            session_id,
            payload,
        } => {
            if let Some(sess) = state.sessions.get(&session_id) {
                if !sess.established {
                    return;
                }
                let forward = SignalMessage::EncryptedRelay {
                    session_id: session_id.clone(),
                    payload,
                };
                match role {
                    PeerRole::Host { .. } => send_signal(&sess.viewer_tx.control_tx, &forward),
                    PeerRole::Viewer { .. } => send_signal(&sess.host_tx.control_tx, &forward),
                    PeerRole::Unidentified => {}
                }
            }
        }

        SignalMessage::Disconnect { session_id, reason } => {
            if let Some((_, sess)) = state.sessions.remove(&session_id) {
                info!(session_id = %session_id, ?reason, "Session disconnected");
                let disc = SignalMessage::Disconnect { session_id, reason };
                match role {
                    PeerRole::Host { .. } => send_signal(&sess.viewer_tx.control_tx, &disc),
                    PeerRole::Viewer { .. } => send_signal(&sess.host_tx.control_tx, &disc),
                    PeerRole::Unidentified => {}
                }
            }
        }

        SignalMessage::Ping { timestamp } => {
            send_signal(tx, &SignalMessage::Pong { timestamp });
        }

        other => {
            debug!("Ignoring unexpected message from client: {:?}", other);
        }
    }
}

fn cleanup_peer(role: &PeerRole, state: &Arc<ServerState>) {
    match role {
        PeerRole::Host { device_id } => {
            info!(device_id = %device_id, "Host disconnected");
            state.hosts.remove(device_id);

            // Notify any viewers connected to this host
            let affected: Vec<String> = state
                .sessions
                .iter()
                .filter(|s| s.host_device_id == *device_id)
                .map(|s| s.session_id.clone())
                .collect();

            for sid in affected {
                if let Some((_, sess)) = state.sessions.remove(&sid) {
                    send_signal(
                        &sess.viewer_tx.control_tx,
                        &SignalMessage::Disconnect {
                            session_id: sid,
                            reason: Some("Host went offline".into()),
                        },
                    );
                }
            }
        }
        PeerRole::Viewer { session_id } => {
            if let Some((_, sess)) = state.sessions.remove(session_id) {
                info!(session_id = %session_id, "Viewer disconnected");
                send_signal(
                    &sess.host_tx.control_tx,
                    &SignalMessage::Disconnect {
                        session_id: session_id.clone(),
                        reason: Some("Viewer disconnected".into()),
                    },
                );
            }
        }
        PeerRole::Unidentified => {}
    }
}
