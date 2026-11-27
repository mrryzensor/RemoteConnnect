//! Ultra-low-latency Windows DXGI Desktop Duplication screen capturer.
//!
//! Uses `IDXGIOutputDuplication::AcquireNextFrame` (event-driven by DWM) so that
//! screen updates (e.g. opening a browser tab) are captured within ~1-3 ms with
//! zero per-frame 8 MB heap allocations and 0% CPU usage when idle.

use anyhow::{anyhow, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DxgiCaptureStatus {
    Frame,
    Unchanged,
}

pub struct DxgiCapturer {
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    width: usize,
    height: usize,
    has_frame: bool,
}

impl DxgiCapturer {
    pub fn new(display_index: usize) -> Result<Self> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()
                .map_err(|e| anyhow!("CreateDXGIFactory1 failed: {:?}", e))?;

            let mut target: Option<(IDXGIAdapter1, IDXGIOutput1)> = None;
            let mut seen_outputs = 0usize;
            let mut adapter_idx = 0u32;

            while let Ok(adapter) = factory.EnumAdapters1(adapter_idx) {
                let mut output_idx = 0u32;
                while let Ok(output) = adapter.EnumOutputs(output_idx) {
                    if let Ok(output1) = output.cast::<IDXGIOutput1>() {
                        if seen_outputs == display_index {
                            target = Some((adapter.clone(), output1));
                            break;
                        }
                        if target.is_none() {
                            // Keep first output as fallback if display_index is out of range
                            target = Some((adapter.clone(), output1));
                        }
                        seen_outputs += 1;
                    }
                    output_idx += 1;
                }
                if seen_outputs > display_index {
                    break;
                }
                adapter_idx += 1;
            }

            let (adapter, output1) =
                target.ok_or_else(|| anyhow!("No DXGI output available for display {}", display_index))?;

            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;

            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| anyhow!("D3D11CreateDevice failed: {:?}", e))?;

            let device = device.ok_or_else(|| anyhow!("D3D11CreateDevice returned null device"))?;
            let context =
                context.ok_or_else(|| anyhow!("D3D11CreateDevice returned null context"))?;

            let duplication = output1
                .DuplicateOutput(&device)
                .map_err(|e| anyhow!("DuplicateOutput failed: {:?}", e))?;

            let desc = duplication.GetDesc();
            let width = desc.ModeDesc.Width as usize;
            let height = desc.ModeDesc.Height as usize;
            if width == 0 || height == 0 {
                return Err(anyhow!("DXGI output reported zero dimensions"));
            }

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width as u32,
                Height: height as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let mut staging: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|e| anyhow!("CreateTexture2D staging failed: {:?}", e))?;
            let staging = staging.ok_or_else(|| anyhow!("Staging texture was null"))?;

            Ok(Self {
                _device: device,
                context,
                duplication,
                staging,
                width,
                height,
                has_frame: false,
            })
        }
    }

    /// Wait up to `timeout_ms` for a new DWM-composited desktop frame and pass the mapped
    /// BGRA buffer `(bytes, width, height, row_pitch)` directly to `on_frame`.
    pub fn capture_frame<F>(
        &mut self,
        timeout_ms: u32,
        force_read: bool,
        on_frame: F,
    ) -> Result<DxgiCaptureStatus>
    where
        F: FnOnce(&[u8], usize, usize, usize),
    {
        unsafe {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;

            match self
                .duplication
                .AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource)
            {
                Ok(()) => {
                    let has_pixel_update = frame_info.AccumulatedFrames > 0
                        || frame_info.LastPresentTime != 0
                        || !self.has_frame
                        || force_read;

                    if !has_pixel_update {
                        let _ = self.duplication.ReleaseFrame();
                        return Ok(DxgiCaptureStatus::Unchanged);
                    }

                    let copy_res = (|| -> Result<()> {
                        let res = resource.ok_or_else(|| anyhow!("DXGI resource was null"))?;
                        let tex: ID3D11Texture2D = res
                            .cast()
                            .map_err(|e| anyhow!("Cast to ID3D11Texture2D failed: {:?}", e))?;
                        self.context.CopyResource(&self.staging, &tex);
                        Ok(())
                    })();

                    let _ = self.duplication.ReleaseFrame();
                    copy_res?;
                    self.has_frame = true;
                }
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    if !(force_read && self.has_frame) {
                        return Ok(DxgiCaptureStatus::Unchanged);
                    }
                }
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    return Err(anyhow!("DXGI_ERROR_ACCESS_LOST"));
                }
                Err(e) => {
                    return Err(anyhow!("AcquireNextFrame failed: {:?}", e));
                }
            }

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| anyhow!("ID3D11DeviceContext::Map failed: {:?}", e))?;

            let row_pitch = mapped.RowPitch as usize;
            let total_bytes = row_pitch * self.height;
            let slice = std::slice::from_raw_parts(mapped.pData as *const u8, total_bytes);

            on_frame(slice, self.width, self.height, row_pitch);

            self.context.Unmap(&self.staging, 0);
            Ok(DxgiCaptureStatus::Frame)
        }
    }
}
