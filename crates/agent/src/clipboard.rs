use std::fs;
use std::path::Path;

use arboard::Clipboard;
use remoteconnect_protocol::ClipboardFileInfo;

pub fn get_clipboard_text() -> Option<String> {
    let mut cb = Clipboard::new().ok()?;
    cb.get_text().ok()
}

pub fn set_clipboard_text(text: &str) -> bool {
    if let Ok(mut cb) = Clipboard::new() {
        cb.set_text(text.to_string()).is_ok()
    } else {
        false
    }
}

/// Read file paths currently copied on the OS clipboard (Ctrl+C in Windows Explorer).
#[cfg(target_os = "windows")]
pub fn get_clipboard_files() -> Vec<ClipboardFileInfo> {
    use clipboard_win::{formats, get_clipboard};

    let Ok(paths): Result<Vec<String>, _> = get_clipboard(formats::FileList) else {
        return vec![];
    };

    paths
        .into_iter()
        .filter_map(|p| {
            let path = Path::new(&p);
            let meta = fs::metadata(path).ok()?;
            if meta.is_dir() {
                return None;
            }
            let name = path.file_name()?.to_string_lossy().to_string();
            Some(ClipboardFileInfo {
                name,
                path: p,
                size: meta.len(),
            })
        })
        .collect()
}

#[cfg(not(target_os = "windows"))]
pub fn get_clipboard_files() -> Vec<ClipboardFileInfo> {
    vec![]
}

/// Place one or more file paths onto the Windows clipboard (CF_HDROP) so Ctrl+V pastes them anywhere!
#[cfg(target_os = "windows")]
pub fn set_clipboard_files(paths: &[String]) -> bool {
    use clipboard_win::{raw, Clipboard};
    if paths.is_empty() {
        return false;
    }
    let Ok(_clip) = Clipboard::new_attempts(10) else {
        return false;
    };
    raw::set_file_list(paths).is_ok()
}

#[cfg(not(target_os = "windows"))]
pub fn set_clipboard_files(_paths: &[String]) -> bool {
    false
}
