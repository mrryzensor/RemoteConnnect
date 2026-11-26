use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use tokio::sync::mpsc;
use tracing::{info, warn};

/// A chunk of captured system audio (interleaved i16 LE PCM).
pub struct AudioChunk {
    pub sample_rate: u32,
    pub channels: u16,
    pub pcm_i16_le: Vec<u8>,
}

/// Spawns a dedicated OS thread capturing system playback audio (WASAPI loopback on Windows,
/// default input fallback elsewhere) and sends ~40ms PCM chunks until `active` becomes false.
pub fn spawn_audio_capture(active: Arc<AtomicBool>) -> mpsc::UnboundedReceiver<AudioChunk> {
    let (tx, rx) = mpsc::unbounded_channel();

    std::thread::spawn(move || {
        let host = cpal::default_host();

        // On Windows (WASAPI), opening an input stream on the default OUTPUT device captures system loopback audio!
        #[cfg(target_os = "windows")]
        let device_opt = host.default_output_device().or_else(|| host.default_input_device());

        #[cfg(not(target_os = "windows"))]
        let device_opt = host.default_input_device();

        let Some(device) = device_opt else {
            warn!("No audio device available for remote audio streaming");
            return;
        };

        let config_res = {
            #[cfg(target_os = "windows")]
            {
                device
                    .default_output_config()
                    .or_else(|_| device.default_input_config())
            }
            #[cfg(not(target_os = "windows"))]
            {
                device.default_input_config()
            }
        };

        let Ok(supported) = config_res else {
            warn!("Failed to query default audio stream config");
            return;
        };

        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels();
        let sample_format = supported.sample_format();
        let stream_config: StreamConfig = supported.into();

        info!(
            "Starting remote audio capture: {} Hz, {} ch, {:?}",
            sample_rate, channels, sample_format
        );

        let err_fn = |err| warn!("Audio stream error: {}", err);

        // Buffer roughly 40ms of audio before emitting a network packet
        let target_samples = ((sample_rate as usize) * (channels as usize) * 40) / 1000;
        let mut accum: Vec<u8> = Vec::with_capacity(target_samples * 2);

        let stream_res = match sample_format {
            SampleFormat::F32 => {
                let tx_c = tx.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| {
                        for &s in data {
                            let clamped = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                            accum.extend_from_slice(&clamped.to_le_bytes());
                        }
                        if accum.len() >= target_samples * 2 {
                            let payload = std::mem::take(&mut accum);
                            let _ = tx_c.send(AudioChunk {
                                sample_rate,
                                channels,
                                pcm_i16_le: payload,
                            });
                        }
                    },
                    err_fn,
                    None,
                )
            }
            SampleFormat::I16 => {
                let tx_c = tx.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _| {
                        for &s in data {
                            accum.extend_from_slice(&s.to_le_bytes());
                        }
                        if accum.len() >= target_samples * 2 {
                            let payload = std::mem::take(&mut accum);
                            let _ = tx_c.send(AudioChunk {
                                sample_rate,
                                channels,
                                pcm_i16_le: payload,
                            });
                        }
                    },
                    err_fn,
                    None,
                )
            }
            SampleFormat::U16 => {
                let tx_c = tx.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[u16], _| {
                        for &s in data {
                            let signed = (s as i32 - 32768) as i16;
                            accum.extend_from_slice(&signed.to_le_bytes());
                        }
                        if accum.len() >= target_samples * 2 {
                            let payload = std::mem::take(&mut accum);
                            let _ = tx_c.send(AudioChunk {
                                sample_rate,
                                channels,
                                pcm_i16_le: payload,
                            });
                        }
                    },
                    err_fn,
                    None,
                )
            }
            _ => {
                warn!("Unsupported audio sample format: {:?}", sample_format);
                return;
            }
        };

        let Ok(stream) = stream_res else {
            warn!("Failed to build audio capture stream");
            return;
        };

        if let Err(e) = stream.play() {
            warn!("Failed to start audio stream: {}", e);
            return;
        }

        while active.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }

        drop(stream);
        info!("Remote audio capture stopped");
    });

    rx
}
