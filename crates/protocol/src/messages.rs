use serde::{Deserialize, Serialize};

/// Top-level messages exchanged over the WebSocket connection between
/// clients (Host Agents or Viewers) and the Rust Relay/Signaling Server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalMessage {
    /// Host Agent registers itself with the server.
    RegisterHost {
        device_id: String,
        device_name: String,
        os: String,
        /// Optional server auth token if the server requires one.
        auth_token: Option<String>,
        /// Whether unattended PIN access is enabled on this host.
        unattended_enabled: bool,
    },

    /// Server acknowledges host registration.
    Registered {
        device_id: String,
        server_version: String,
    },

    /// Viewer requests a remote session with a target host.
    ConnectRequest {
        target_device_id: String,
        viewer_name: String,
        /// Optional PIN for unattended access (verified by the host, never stored on server).
        pin: Option<String>,
        /// Viewer's ephemeral P-256 public key (Base64 uncompressed SEC1) for E2E encryption.
        public_key: String,
        /// Optional server auth token.
        auth_token: Option<String>,
    },

    /// Server forwards an incoming connection request to the Host Agent.
    IncomingConnection {
        session_id: String,
        viewer_name: String,
        pin: Option<String>,
        viewer_public_key: String,
    },

    /// Host Agent responds to an incoming connection request (after PIN check or user approval).
    ConnectResponse {
        session_id: String,
        accepted: bool,
        reason: Option<String>,
        /// Host's ephemeral P-256 public key (Base64 uncompressed SEC1) for E2E encryption.
        host_public_key: Option<String>,
        /// Initial display info if accepted.
        displays: Vec<DisplayInfo>,
        /// Granted permissions for this session.
        #[serde(default)]
        permissions: SessionPermissions,
    },

    /// Server notifies the Viewer that the session was accepted or rejected.
    SessionReady {
        session_id: String,
        accepted: bool,
        reason: Option<String>,
        host_public_key: Option<String>,
        host_name: Option<String>,
        displays: Vec<DisplayInfo>,
        #[serde(default)]
        permissions: SessionPermissions,
    },

    /// End-to-end encrypted payload relayed blindly by the server between Host and Viewer.
    /// `payload` is Base64-encoded `[12-byte nonce || AES-256-GCM ciphertext + tag]`
    /// containing a serialized `SessionMessage`.
    EncryptedRelay {
        session_id: String,
        payload: String,
    },

    /// Terminate an active session.
    Disconnect {
        session_id: String,
        reason: Option<String>,
    },

    /// Keep-alive ping/pong.
    Ping { timestamp: u64 },
    Pong { timestamp: u64 },

    /// Protocol or server error.
    Error { code: String, message: String },
}

/// Granular permissions granted by the Host for a remote session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionPermissions {
    pub allow_input: bool,
    pub allow_audio: bool,
    pub allow_clipboard: bool,
    pub allow_file_transfer: bool,
}

impl Default for SessionPermissions {
    fn default() -> Self {
        Self {
            allow_input: true,
            allow_audio: true,
            allow_clipboard: true,
            allow_file_transfer: true,
        }
    }
}

/// Information about a file currently on the remote clipboard (Ctrl+C).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardFileInfo {
    pub name: String,
    pub path: String,
    pub size: u64,
}

/// Information about a physical monitor on the Host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

/// End-to-end encrypted messages exchanged directly between Host and Viewer
/// inside `SignalMessage::EncryptedRelay` (or encrypted binary WebSocket frames).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionMessage {
    /// Video frame captured from the Host screen.
    VideoFrame {
        width: u32,
        height: u32,
        /// Encoding format: "jpeg"
        format: String,
        /// Base64-encoded image bytes.
        data: String,
        timestamp: u64,
    },

    /// Viewer requests stream quality / FPS / display changes.
    StreamConfig {
        fps: u8,
        quality: u8,
        display_index: usize,
        scale: f32,
        #[serde(default)]
        bitrate_kbps: u32,
    },

    /// Viewer requests an immediate H.264 IDR keyframe.
    RequestKeyframe,

    /// Mouse movement (normalized 0.0..=1.0 coordinates).
    MouseMove { x: f64, y: f64 },

    /// Mouse button press/release.
    MouseButton {
        button: MouseBtn,
        pressed: bool,
        x: f64,
        y: f64,
    },

    /// Mouse scroll wheel.
    MouseScroll { delta_x: i32, delta_y: i32 },

    /// Keyboard key press/release.
    KeyEvent {
        /// Standard browser `KeyboardEvent.code` or key string (e.g., "KeyA", "Enter", "ControlLeft").
        code: String,
        /// Printable key value (e.g., "a", "A", "Enter").
        key: String,
        pressed: bool,
    },

    /// Clipboard text synchronization (bidirectional).
    ClipboardSync { text: String },

    /// Host notifies Viewer that file(s) were copied to Host clipboard (Ctrl+C).
    ClipboardFilesAvailable { files: Vec<ClipboardFileInfo> },

    /// Chunk of a file pasted via Ctrl+V from Viewer -> Host.
    /// When `paste_after_receive` is true, Host places file in clipboard (CF_HDROP) and triggers Ctrl+V.
    ClipboardFilePaste {
        transfer_id: String,
        file_name: String,
        offset: u64,
        total_size: u64,
        data: String,
        is_last: bool,
        paste_after_receive: bool,
    },

    /// Request directory listing from Host.
    FileListRequest { path: Option<String> },

    /// Directory listing response from Host.
    FileListResponse {
        current_path: String,
        parent_path: Option<String>,
        entries: Vec<FileEntry>,
        error: Option<String>,
    },

    /// Viewer requests downloading a file from Host.
    FileDownloadRequest { path: String },

    /// Chunk of a file being transferred (Host -> Viewer or Viewer -> Host).
    FileChunk {
        transfer_id: String,
        file_name: String,
        /// Target directory when uploading to host.
        target_dir: Option<String>,
        offset: u64,
        total_size: u64,
        /// Base64-encoded chunk bytes.
        data: String,
        is_last: bool,
    },

    /// Acknowledgement / status for file transfers.
    FileTransferStatus {
        transfer_id: String,
        ok: bool,
        message: String,
    },

    /// WebRTC P2P SDP Offer (Viewer -> Host, relayed E2E over signaling server).
    RtcOffer { sdp: String },

    /// WebRTC P2P SDP Answer (Host -> Viewer, relayed E2E over signaling server).
    RtcAnswer { sdp: String },

    /// WebRTC P2P Trickle ICE candidate (bidirectional, relayed E2E over signaling server).
    RtcIceCandidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },

    /// Direct P2P RTT measurement ping/pong over WebRTC control DataChannel.
    P2pPing { timestamp: u64 },
    P2pPong { timestamp: u64 },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseBtn {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
}
