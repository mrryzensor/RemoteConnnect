//! Direct WebRTC P2P DataChannel transport for ultra-low-latency remote desktop.
//!
//! Uses `remote.2mail.us` only for E2E-encrypted SDP/ICE signaling. Once connected
//! over LAN (< 5 ms) or direct Internet UDP (15-35 ms via Cloudflare/Google STUN),
//! video, audio, and input bypass the relay server completely. Automatically falls
//! back to the WebSocket relay if P2P cannot connect or drops.

use std::sync::{
    atomic::{AtomicBool, AtomicU16, Ordering},
    Arc,
};

use bytes::Bytes;
use remoteconnect_protocol::{E2eSession, SessionMessage};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{info, warn};
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

use crate::send_encrypted_session_msg;

/// Safe chunk payload size for SCTP DataChannels across all browsers/WebView2.
const P2P_CHUNK_SIZE: usize = 32 * 1024;

pub struct P2pManager {
    session_id: String,
    e2e: Arc<E2eSession>,
    ws_out_tx: mpsc::UnboundedSender<WsMessage>,
    pc: Arc<Mutex<Option<Arc<RTCPeerConnection>>>>,
    pending_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>>,
    remote_desc_set: Arc<AtomicBool>,
    media_ready: Arc<AtomicBool>,
    control_ready: Arc<AtomicBool>,
    media_tx: mpsc::Sender<Vec<u8>>,
    media_dc_slot: Arc<Mutex<Option<Arc<RTCDataChannel>>>>,
    control_dc_slot: Arc<Mutex<Option<Arc<RTCDataChannel>>>>,
    on_control_msg: Arc<dyn Fn(SessionMessage) + Send + Sync>,
    on_media_ready: Arc<dyn Fn() + Send + Sync>,
}

impl P2pManager {
    /// Create a P2P manager for a host session.
    pub fn spawn<F, G>(
        session_id: String,
        e2e: Arc<E2eSession>,
        ws_out_tx: mpsc::UnboundedSender<WsMessage>,
        on_control_msg: F,
        on_media_ready: G,
    ) -> Arc<Self>
    where
        F: Fn(SessionMessage) + Send + Sync + 'static,
        G: Fn() + Send + Sync + 'static,
    {
        let (media_tx, mut media_rx) = mpsc::channel::<Vec<u8>>(2);
        let media_dc_slot: Arc<Mutex<Option<Arc<RTCDataChannel>>>> = Arc::new(Mutex::new(None));
        let control_dc_slot: Arc<Mutex<Option<Arc<RTCDataChannel>>>> = Arc::new(Mutex::new(None));
        let media_ready = Arc::new(AtomicBool::new(false));
        let control_ready = Arc::new(AtomicBool::new(false));
        let pc_slot: Arc<Mutex<Option<Arc<RTCPeerConnection>>>> = Arc::new(Mutex::new(None));
        let pending_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>> =
            Arc::new(Mutex::new(Vec::new()));
        let remote_desc_set = Arc::new(AtomicBool::new(false));
        let next_msg_id = Arc::new(AtomicU16::new(1));

        // Pump media packets -> fragmented SCTP DataChannel messages
        {
            let media_dc_for_pump = media_dc_slot.clone();
            let media_ready_for_pump = media_ready.clone();
            let msg_id_gen = next_msg_id.clone();
            tokio::spawn(async move {
                while let Some(raw_packet) = media_rx.recv().await {
                    if !media_ready_for_pump.load(Ordering::Relaxed) {
                        continue;
                    }
                    let dc_opt = media_dc_for_pump.lock().await.clone();
                    let Some(dc) = dc_opt else {
                        continue;
                    };
                    let msg_id = msg_id_gen.fetch_add(1, Ordering::Relaxed);
                    let total_chunks =
                        ((raw_packet.len() + P2P_CHUNK_SIZE - 1) / P2P_CHUNK_SIZE).max(1);
                    if total_chunks > u16::MAX as usize {
                        continue;
                    }
                    let total_u16 = total_chunks as u16;
                    let mut failed = false;
                    for (idx, chunk) in raw_packet.chunks(P2P_CHUNK_SIZE).enumerate() {
                        let mut framed = Vec::with_capacity(6 + chunk.len());
                        framed.extend_from_slice(&msg_id.to_be_bytes());
                        framed.extend_from_slice(&(idx as u16).to_be_bytes());
                        framed.extend_from_slice(&total_u16.to_be_bytes());
                        framed.extend_from_slice(chunk);
                        if let Err(e) = dc.send(&Bytes::from(framed)).await {
                            warn!("P2P media send failed (falling back to relay): {}", e);
                            media_ready_for_pump.store(false, Ordering::Release);
                            failed = true;
                            break;
                        }
                    }
                    if failed {
                        *media_dc_for_pump.lock().await = None;
                    }
                }
            });
        }

        Arc::new(Self {
            session_id,
            e2e,
            ws_out_tx,
            pc: pc_slot,
            pending_candidates,
            remote_desc_set,
            media_ready,
            control_ready,
            media_tx,
            media_dc_slot,
            control_dc_slot,
            on_control_msg: Arc::new(on_control_msg),
            on_media_ready: Arc::new(on_media_ready),
        })
    }
    #[inline]
    pub fn is_media_ready(&self) -> bool {
        self.media_ready.load(Ordering::Acquire)
    }

    #[inline]
    pub fn media_capacity(&self) -> usize {
        self.media_tx.capacity()
    }

    /// Blocking send from the dedicated capture OS thread. Returns `true` if sent via P2P.
    pub fn send_media_blocking(&self, raw_packet: Vec<u8>) -> bool {
        if !self.is_media_ready() {
            return false;
        }
        self.media_tx.blocking_send(raw_packet).is_ok()
    }

    /// Non-blocking try_send for audio chunks from async tasks. Returns `true` if queued on P2P.
    pub fn try_send_media(&self, raw_packet: Vec<u8>) -> bool {
        if !self.is_media_ready() {
            return false;
        }
        self.media_tx.try_send(raw_packet).is_ok()
    }

    /// Send a control `SessionMessage` directly over the P2P control DataChannel if open.
    pub fn send_control_if_ready(self: &Arc<Self>, msg: &SessionMessage) -> bool {
        if !self.control_ready.load(Ordering::Acquire) {
            return false;
        }
        let Ok(json) = serde_json::to_string(msg) else {
            return false;
        };
        let slot = self.control_dc_slot.clone();
        let ready = self.control_ready.clone();
        tokio::spawn(async move {
            let dc_opt = slot.lock().await.clone();
            if let Some(dc) = dc_opt {
                if dc.send_text(json).await.is_err() {
                    ready.store(false, Ordering::Release);
                }
            }
        });
        true
    }

    /// Process an incoming WebRTC SDP offer from the viewer and send back an SDP answer.
    pub fn handle_offer(self: &Arc<Self>, sdp: String) {
        let mgr = self.clone();
        tokio::spawn(async move {
            if let Err(e) = mgr.handle_offer_inner(sdp).await {
                warn!("WebRTC P2P offer handling failed (keeping relay): {}", e);
            }
        });
    }

    async fn handle_offer_inner(self: &Arc<Self>, sdp: String) -> anyhow::Result<()> {
        // Close any previous peer connection
        if let Some(old_pc) = self.pc.lock().await.take() {
            let _ = old_pc.close().await;
        }
        self.media_ready.store(false, Ordering::Release);
        self.control_ready.store(false, Ordering::Release);
        self.remote_desc_set.store(false, Ordering::Release);

        let api = APIBuilder::new().build();
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec![
                    "stun:stun.cloudflare.com:3478".to_string(),
                    "stun:stun.l.google.com:19302".to_string(),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };

        let pc = Arc::new(api.new_peer_connection(config).await?);

        // Send local ICE candidates back to viewer via E2E WebSocket signaling
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

        // Monitor connection state for automatic fallback to relay
        {
            let media_ready = self.media_ready.clone();
            let control_ready = self.control_ready.clone();
            let media_slot = self.media_dc_slot.clone();
            let control_slot = self.control_dc_slot.clone();
            let sid = self.session_id.clone();
            pc.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
                let media_ready = media_ready.clone();
                let control_ready = control_ready.clone();
                let media_slot = media_slot.clone();
                let control_slot = control_slot.clone();
                let sid = sid.clone();
                Box::pin(async move {
                    info!(session_id = %sid, state = ?state, "WebRTC P2P state changed");
                    if matches!(
                        state,
                        RTCPeerConnectionState::Disconnected
                            | RTCPeerConnectionState::Failed
                            | RTCPeerConnectionState::Closed
                    ) {
                        media_ready.store(false, Ordering::Release);
                        control_ready.store(false, Ordering::Release);
                        *media_slot.lock().await = None;
                        *control_slot.lock().await = None;
                    }
                })
            }));
        }

        // Handle incoming DataChannels created by the viewer ("control" and "media")
        {
            let media_ready = self.media_ready.clone();
            let control_ready = self.control_ready.clone();
            let media_slot = self.media_dc_slot.clone();
            let control_slot = self.control_dc_slot.clone();
            let on_control = self.on_control_msg.clone();
            let on_media_ready = self.on_media_ready.clone();
            let sid = self.session_id.clone();

            pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
                let label = dc.label().to_string();
                let media_ready = media_ready.clone();
                let control_ready = control_ready.clone();
                let media_slot = media_slot.clone();
                let control_slot = control_slot.clone();
                let on_control = on_control.clone();
                let on_media_ready = on_media_ready.clone();
                let sid = sid.clone();

                Box::pin(async move {
                    if label == "media" {
                        let dc_open = dc.clone();
                        let media_slot_open = media_slot.clone();
                        let media_ready_open = media_ready.clone();
                        let on_ready = on_media_ready.clone();
                        let sid_open = sid.clone();
                        dc.on_open(Box::new(move || {
                            let dc_open = dc_open.clone();
                            let media_slot_open = media_slot_open.clone();
                            let media_ready_open = media_ready_open.clone();
                            let on_ready = on_ready.clone();
                            let sid_open = sid_open.clone();
                            Box::pin(async move {
                                info!(session_id = %sid_open, "P2P media DataChannel OPEN — switching video/audio to direct P2P!");
                                *media_slot_open.lock().await = Some(dc_open);
                                media_ready_open.store(true, Ordering::Release);
                                on_ready();
                            })
                        }));

                        let media_slot_close = media_slot.clone();
                        let media_ready_close = media_ready.clone();
                        dc.on_close(Box::new(move || {
                            let media_slot_close = media_slot_close.clone();
                            let media_ready_close = media_ready_close.clone();
                            Box::pin(async move {
                                media_ready_close.store(false, Ordering::Release);
                                *media_slot_close.lock().await = None;
                            })
                        }));
                    } else if label == "control" {
                        let dc_open = dc.clone();
                        let control_slot_open = control_slot.clone();
                        let control_ready_open = control_ready.clone();
                        let sid_open = sid.clone();
                        dc.on_open(Box::new(move || {
                            let dc_open = dc_open.clone();
                            let control_slot_open = control_slot_open.clone();
                            let control_ready_open = control_ready_open.clone();
                            let sid_open = sid_open.clone();
                            Box::pin(async move {
                                info!(session_id = %sid_open, "P2P control DataChannel OPEN");
                                *control_slot_open.lock().await = Some(dc_open);
                                control_ready_open.store(true, Ordering::Release);
                            })
                        }));

                        let control_slot_close = control_slot.clone();
                        let control_ready_close = control_ready.clone();
                        dc.on_close(Box::new(move || {
                            let control_slot_close = control_slot_close.clone();
                            let control_ready_close = control_ready_close.clone();
                            Box::pin(async move {
                                control_ready_close.store(false, Ordering::Release);
                                *control_slot_close.lock().await = None;
                            })
                        }));

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

        let offer = RTCSessionDescription::offer(sdp)?;
        pc.set_remote_description(offer).await?;
        self.remote_desc_set.store(true, Ordering::Release);

        // Drain any ICE candidates that arrived before set_remote_description
        let queued: Vec<RTCIceCandidateInit> = {
            let mut q = self.pending_candidates.lock().await;
            std::mem::take(&mut *q)
        };
        for cand in queued {
            let _ = pc.add_ice_candidate(cand).await;
        }

        let answer = pc.create_answer(None).await?;
        let answer_sdp = answer.sdp.clone();
        pc.set_local_description(answer).await?;

        *self.pc.lock().await = Some(pc);

        send_encrypted_session_msg(
            &self.ws_out_tx,
            &self.session_id,
            &self.e2e,
            &SessionMessage::RtcAnswer { sdp: answer_sdp },
        );

        Ok(())
    }

    pub fn handle_ice_candidate(
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
                username_fragment: None,
            };
            if !mgr.remote_desc_set.load(Ordering::Acquire) {
                mgr.pending_candidates.lock().await.push(init);
                return;
            }
            if let Some(pc) = mgr.pc.lock().await.clone() {
                let _ = pc.add_ice_candidate(init).await;
            }
        });
    }

    pub fn close(self: &Arc<Self>) {
        self.media_ready.store(false, Ordering::Release);
        self.control_ready.store(false, Ordering::Release);
        let pc_slot = self.pc.clone();
        let media_slot = self.media_dc_slot.clone();
        let control_slot = self.control_dc_slot.clone();
        tokio::spawn(async move {
            *media_slot.lock().await = None;
            *control_slot.lock().await = None;
            if let Some(pc) = pc_slot.lock().await.take() {
                let _ = pc.close().await;
            }
        });
    }
}
