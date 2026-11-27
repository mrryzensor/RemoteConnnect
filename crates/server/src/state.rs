use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::Message;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::sync::mpsc;

pub type OutboundTx = mpsc::UnboundedSender<Message>;
pub type MediaTx = mpsc::Sender<Message>;

#[derive(Debug, Clone)]
pub struct PeerChannels {
    pub control_tx: OutboundTx,
    pub media_tx: MediaTx,
}

#[derive(Debug, Clone)]
pub struct HostPeer {
    pub device_id: String,
    pub device_name: String,
    pub os: String,
    pub unattended_enabled: bool,
    pub connected_at: DateTime<Utc>,
    pub tx: PeerChannels,
}

#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub session_id: String,
    pub host_device_id: String,
    pub host_tx: PeerChannels,
    pub viewer_tx: PeerChannels,
    pub established: bool,
    pub created_at: DateTime<Utc>,
}

pub struct ServerState {
    pub auth_token: Option<String>,
    pub started_at: Instant,
    /// Online Host Agents indexed by normalized device_id.
    pub hosts: DashMap<String, HostPeer>,
    /// Active & pending sessions indexed by session_id (36-char UUID string).
    pub sessions: DashMap<String, ActiveSession>,
}

impl ServerState {
    pub fn new(auth_token: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            auth_token,
            started_at: Instant::now(),
            hosts: DashMap::new(),
            sessions: DashMap::new(),
        })
    }

    pub fn verify_token(&self, provided: &Option<String>) -> bool {
        match &self.auth_token {
            None => true,
            Some(expected) => provided.as_deref() == Some(expected.as_str()),
        }
    }
}
