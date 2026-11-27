use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use remoteconnect_protocol::MouseBtn;
use tracing::warn;

use crate::capture::DisplayBounds;

pub struct InputController {
    enigo: Enigo,
}

impl InputController {
    pub fn new() -> anyhow::Result<Self> {
        let enigo = Enigo::new(&Settings::default())
            .map_err(|e| anyhow::anyhow!("Failed to initialize input controller: {:?}", e))?;
        Ok(Self { enigo })
    }

    pub fn move_mouse(&mut self, norm_x: f64, norm_y: f64, bounds: DisplayBounds) {
        let abs_x = bounds.x + (norm_x.clamp(0.0, 1.0) * (bounds.width as f64)).round() as i32;
        let abs_y = bounds.y + (norm_y.clamp(0.0, 1.0) * (bounds.height as f64)).round() as i32;
        if let Err(e) = self.enigo.move_mouse(abs_x, abs_y, Coordinate::Abs) {
            warn!("move_mouse failed: {:?}", e);
        }
    }

    pub fn mouse_button(
        &mut self,
        button: MouseBtn,
        pressed: bool,
        norm_x: f64,
        norm_y: f64,
        bounds: DisplayBounds,
    ) {
        self.move_mouse(norm_x, norm_y, bounds);
        let btn = match button {
            MouseBtn::Left => Button::Left,
            MouseBtn::Right => Button::Right,
            MouseBtn::Middle => Button::Middle,
        };
        let dir = if pressed {
            Direction::Press
        } else {
            Direction::Release
        };
        if let Err(e) = self.enigo.button(btn, dir) {
            warn!("mouse_button failed: {:?}", e);
        }
    }

    pub fn mouse_scroll(&mut self, delta_x: i32, delta_y: i32) {
        if delta_y != 0 {
            let _ = self.enigo.scroll(delta_y, Axis::Vertical);
        }
        if delta_x != 0 {
            let _ = self.enigo.scroll(delta_x, Axis::Horizontal);
        }
    }

    pub fn key_event(&mut self, code: &str, key: &str, pressed: bool) {
        let dir = if pressed {
            Direction::Press
        } else {
            Direction::Release
        };

        if let Some(mapped) = map_key(code, key) {
            if let Err(e) = self.enigo.key(mapped, dir) {
                warn!("key_event failed ({}/{}): {:?}", code, key, e);
            }
        }
    }

    /// Synthesize Ctrl+V (or Cmd+V on macOS) so a clipboard-transferred file pastes directly.
    pub fn trigger_paste(&mut self) {
        #[cfg(target_os = "macos")]
        let modifier = Key::Meta;
        #[cfg(not(target_os = "macos"))]
        let modifier = Key::Control;

        let _ = self.enigo.key(modifier, Direction::Press);
        let _ = self.enigo.key(Key::Unicode('v'), Direction::Click);
        let _ = self.enigo.key(modifier, Direction::Release);
    }
}

fn map_key(code: &str, key: &str) -> Option<Key> {
    match code {
        "Enter" | "NumpadEnter" => Some(Key::Return),
        "Tab" => Some(Key::Tab),
        "Backspace" => Some(Key::Backspace),
        "Delete" => Some(Key::Delete),
        "Insert" => Some(Key::Insert),
        "PrintScreen" => Some(Key::PrintScr),
        "Pause" => Some(Key::Pause),
        "ScrollLock" => {
            #[cfg(target_os = "windows")]
            {
                Some(Key::Scroll)
            }
            #[cfg(not(target_os = "windows"))]
            {
                Some(Key::ScrollLock)
            }
        }
        "Escape" => Some(Key::Escape),
        "Space" => Some(Key::Space),
        "ArrowUp" => Some(Key::UpArrow),
        "ArrowDown" => Some(Key::DownArrow),
        "ArrowLeft" => Some(Key::LeftArrow),
        "ArrowRight" => Some(Key::RightArrow),
        "Home" => Some(Key::Home),
        "End" => Some(Key::End),
        "PageUp" => Some(Key::PageUp),
        "PageDown" => Some(Key::PageDown),
        "ShiftLeft" | "ShiftRight" => Some(Key::Shift),
        "ControlLeft" | "ControlRight" => Some(Key::Control),
        "AltLeft" => Some(Key::Alt),
        "AltRight" => {
            #[cfg(target_os = "windows")]
            {
                Some(Key::RMenu)
            }
            #[cfg(not(target_os = "windows"))]
            {
                Some(Key::Alt)
            }
        }
        "MetaLeft" | "MetaRight" => Some(Key::Meta),
        "CapsLock" => Some(Key::CapsLock),
        "F1" => Some(Key::F1),
        "F2" => Some(Key::F2),
        "F3" => Some(Key::F3),
        "F4" => Some(Key::F4),
        "F5" => Some(Key::F5),
        "F6" => Some(Key::F6),
        "F7" => Some(Key::F7),
        "F8" => Some(Key::F8),
        "F9" => Some(Key::F9),
        "F10" => Some(Key::F10),
        "F11" => Some(Key::F11),
        "F12" => Some(Key::F12),
        _ => {
            let mut chars = key.chars();
            if let (Some(ch), None) = (chars.next(), chars.next()) {
                Some(Key::Unicode(ch))
            } else {
                None
            }
        }
    }
}
