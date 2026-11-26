use std::io::Cursor;

use anyhow::{anyhow, Result};
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage};
use remoteconnect_protocol::DisplayInfo;
use xcap::Monitor;

/// List all available physical monitors.
pub fn list_displays() -> Vec<DisplayInfo> {
    let Ok(monitors) = Monitor::all() else {
        return vec![];
    };

    monitors
        .into_iter()
        .enumerate()
        .map(|(idx, m)| DisplayInfo {
            id: m.id().unwrap_or(idx as u32),
            name: m
                .name()
                .unwrap_or_else(|_| format!("Display {}", idx + 1)),
            width: m.width().unwrap_or(1920),
            height: m.height().unwrap_or(1080),
            is_primary: m.is_primary().unwrap_or(idx == 0),
        })
        .collect()
}

/// Geometry of a display in virtual desktop coordinates (for mouse mapping).
#[derive(Debug, Clone, Copy)]
pub struct DisplayBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub fn get_display_bounds(display_index: usize) -> Result<DisplayBounds> {
    let monitors = Monitor::all()?;
    let monitor = monitors
        .get(display_index)
        .or_else(|| monitors.first())
        .ok_or_else(|| anyhow!("No monitors detected"))?;

    Ok(DisplayBounds {
        x: monitor.x().unwrap_or(0),
        y: monitor.y().unwrap_or(0),
        width: monitor.width().unwrap_or(1920),
        height: monitor.height().unwrap_or(1080),
    })
}

/// Capture a single frame from the selected monitor and JPEG-encode it.
/// Returns `(encoded_width, encoded_height, jpeg_bytes)`.
pub fn capture_frame_jpeg(
    display_index: usize,
    scale: f32,
    quality: u8,
) -> Result<(u32, u32, Vec<u8>)> {
    let monitors = Monitor::all()?;
    let monitor = monitors
        .get(display_index)
        .or_else(|| monitors.first())
        .ok_or_else(|| anyhow!("No monitors found"))?;

    let rgba = monitor.capture_image()?;
    let mut width = rgba.width();
    let mut height = rgba.height();

    let dyn_img = DynamicImage::ImageRgba8(rgba);
    let rgb = if scale < 0.99 && scale >= 0.25 {
        width = ((width as f32) * scale).round().max(320.0) as u32;
        height = ((height as f32) * scale).round().max(180.0) as u32;
        dyn_img.resize(width, height, FilterType::Triangle).to_rgb8()
    } else {
        dyn_img.to_rgb8()
    };

    let actual_w = rgb.width();
    let actual_h = rgb.height();

    let mut buf = Vec::with_capacity(64 * 1024);
    let mut encoder = JpegEncoder::new_with_quality(Cursor::new(&mut buf), quality.clamp(15, 95));
    encoder.encode_image(&rgb)?;

    Ok((actual_w, actual_h, buf))
}
