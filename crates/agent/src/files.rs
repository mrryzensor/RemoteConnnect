use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::{DateTime, Utc};
use directories::UserDirs;
use remoteconnect_protocol::{FileEntry, SessionMessage};
use uuid::Uuid;

const CHUNK_SIZE: usize = 128 * 1024; // 128 KB chunks

pub fn default_browse_dir() -> PathBuf {
    if let Some(user_dirs) = UserDirs::new() {
        return user_dirs.home_dir().to_path_buf();
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn default_download_dir() -> PathBuf {
    if let Some(user_dirs) = UserDirs::new() {
        if let Some(dl) = user_dirs.download_dir() {
            return dl.to_path_buf();
        }
        return user_dirs.home_dir().to_path_buf();
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn list_directory(path: Option<String>) -> SessionMessage {
    let target = match path {
        Some(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => default_browse_dir(),
    };

    let canonical = target.canonicalize().unwrap_or_else(|_| target.clone());
    let current_path = canonical.to_string_lossy().to_string();
    let parent_path = canonical
        .parent()
        .map(|p| p.to_string_lossy().to_string());

    let read_dir = match fs::read_dir(&canonical) {
        Ok(rd) => rd,
        Err(e) => {
            return SessionMessage::FileListResponse {
                current_path,
                parent_path,
                entries: vec![],
                error: Some(format!("Cannot read directory: {}", e)),
            };
        }
    };

    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = meta
            .and_then(|m| m.modified().ok())
            .map(|st| {
                let dt: DateTime<Utc> = st.into();
                dt.format("%Y-%m-%d %H:%M").to_string()
            });

        entries.push(FileEntry {
            name,
            path: path.to_string_lossy().to_string(),
            is_dir,
            size,
            modified,
        });
    }

    // Sort directories first, then alphabetically
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    SessionMessage::FileListResponse {
        current_path,
        parent_path,
        entries,
        error: None,
    }
}

/// Read a file into a vector of `SessionMessage::FileChunk` messages for sending to Viewer.
pub fn prepare_file_download(path_str: &str) -> Result<Vec<SessionMessage>, String> {
    let path = Path::new(path_str);
    let meta = fs::metadata(path).map_err(|e| format!("Cannot access file: {}", e))?;
    if meta.is_dir() {
        return Err("Cannot download a directory directly".into());
    }
    let total_size = meta.len();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download.bin".into());

    let mut file = File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
    let transfer_id = Uuid::new_v4().to_string();

    let mut chunks = Vec::new();
    let mut offset = 0u64;
    let mut buf = vec![0u8; CHUNK_SIZE];

    if total_size == 0 {
        chunks.push(SessionMessage::FileChunk {
            transfer_id,
            file_name,
            target_dir: None,
            offset: 0,
            total_size: 0,
            data: String::new(),
            is_last: true,
        });
        return Ok(chunks);
    }

    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("Read error: {}", e))?;
        if n == 0 {
            break;
        }
        let is_last = offset + (n as u64) >= total_size;
        chunks.push(SessionMessage::FileChunk {
            transfer_id: transfer_id.clone(),
            file_name: file_name.clone(),
            target_dir: None,
            offset,
            total_size,
            data: B64.encode(&buf[..n]),
            is_last,
        });
        offset += n as u64;
        if is_last {
            break;
        }
    }

    Ok(chunks)
}

/// Tracks active incoming file uploads from Viewer -> Host.
#[derive(Default)]
pub struct UploadManager {
    active_paths: HashMap<String, PathBuf>,
}

impl UploadManager {
    /// Returns `(optional_status_message, optional_completed_file_path)`.
    pub fn handle_chunk(
        &mut self,
        transfer_id: String,
        file_name: String,
        target_dir: Option<String>,
        offset: u64,
        data_b64: String,
        is_last: bool,
    ) -> (Option<SessionMessage>, Option<PathBuf>) {
        // Sanitize filename to prevent path traversal
        let safe_name = Path::new(&file_name)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "uploaded_file".into());

        let dest_path = self
            .active_paths
            .entry(transfer_id.clone())
            .or_insert_with(|| {
                let dir = target_dir
                    .filter(|d| !d.trim().is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(default_download_dir);
                dir.join(&safe_name)
            })
            .clone();

        let bytes = match B64.decode(&data_b64) {
            Ok(b) => b,
            Err(e) => {
                self.active_paths.remove(&transfer_id);
                return (
                    Some(SessionMessage::FileTransferStatus {
                        transfer_id,
                        ok: false,
                        message: format!("Invalid base64 chunk: {}", e),
                    }),
                    None,
                );
            }
        };

        let write_res = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(offset == 0)
                .open(&dest_path)?;
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(&bytes)?;
            Ok(())
        })();

        if let Err(e) = write_res {
            self.active_paths.remove(&transfer_id);
            return (
                Some(SessionMessage::FileTransferStatus {
                    transfer_id,
                    ok: false,
                    message: format!("Failed to write file: {}", e),
                }),
                None,
            );
        }

        if is_last {
            self.active_paths.remove(&transfer_id);
            return (
                Some(SessionMessage::FileTransferStatus {
                    transfer_id,
                    ok: true,
                    message: format!("Saved to {}", dest_path.display()),
                }),
                Some(dest_path),
            );
        }

        (None, None)
    }
}
