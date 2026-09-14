//! Size probe: the av1-video spike's window (text + image) with patched Slint
//! features. Usage: slint-size-spike [run_secs] [snapshot.pam]
//! Opens for `run_secs` (default 2). With a path, it reads the rendered frame
//! back with glReadPixels one second before quitting (at 1 s minimum).
//! `Window::take_snapshot` returned an all-zero buffer here, patched or not.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use glow::HasContext;
use slint::ComponentHandle;

slint::include_modules!();

const LABEL: &str = "size probe: patched Slint — Latin, العربية, 日本語, ไทย";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let run_secs: u64 = std::env::args().nth(1).map_or(Ok(2), |s| s.parse())?;
    let snapshot = std::env::args().nth(2).map(PathBuf::from);
    slint::BackendSelector::new().require_opengl_es().select()?;
    let window = SpikeWindow::new()?;
    window.set_stats(LABEL.into());

    let capture_requested = Rc::new(Cell::new(false));
    let snapshot_timer = slint::Timer::default();
    if let Some(path) = snapshot {
        install_readback(&window, capture_requested.clone(), path)?;
        let weak = window.as_weak();
        let snapshot_at = Duration::from_secs(run_secs.saturating_sub(1).max(1));
        snapshot_timer.start(slint::TimerMode::SingleShot, snapshot_at, move || {
            capture_requested.set(true);
            if let Some(window) = weak.upgrade() {
                window.window().request_redraw();
            }
        });
    }
    let quit_timer = slint::Timer::default();
    quit_timer.start(slint::TimerMode::SingleShot, Duration::from_secs(run_secs), || {
        slint::quit_event_loop().expect("event loop running");
    });
    window.run()?;
    Ok(())
}

/// Captures the next rendered frame after `requested` is set, before the swap.
fn install_readback(
    window: &SpikeWindow,
    requested: Rc<Cell<bool>>,
    path: PathBuf,
) -> Result<(), slint::SetRenderingNotifierError> {
    let mut gl: Option<glow::Context> = None;
    let weak = window.as_weak();
    window.window().set_rendering_notifier(move |state, graphics_api| match state {
        slint::RenderingState::RenderingSetup => {
            if let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api {
                // SAFETY: Slint's context is current during setup, and
                // get_proc_address resolves symbols for exactly that context.
                gl = Some(unsafe { glow::Context::from_loader_function_cstr(|s| get_proc_address(s)) });
            }
        }
        slint::RenderingState::AfterRendering => {
            if !requested.replace(false) {
                return;
            }
            let (Some(gl), Some(window)) = (gl.as_ref(), weak.upgrade()) else { return };
            let size = window.window().size();
            let pixels = read_back(gl, size.width, size.height);
            write_pam_flipped(&path, size.width, size.height, &pixels).expect("write snapshot");
        }
        _ => {}
    })
}

/// Reads the just-rendered back buffer as RGBA rows, bottom row first.
fn read_back(gl: &glow::Context, width_px: u32, height_px: u32) -> Vec<u8> {
    assert!(width_px > 0 && height_px > 0);
    let mut pixels = vec![0u8; width_px as usize * height_px as usize * 4];
    // SAFETY: called in AfterRendering with Slint's context current; `pixels`
    // holds exactly width × height RGBA bytes at PACK_ALIGNMENT 1; the read
    // framebuffer binding and pack alignment are restored afterwards.
    unsafe {
        let saved_read = gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING);
        let saved_alignment = gl.get_parameter_i32(glow::PACK_ALIGNMENT);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
        gl.read_pixels(
            0,
            0,
            width_px as i32,
            height_px as i32,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixels)),
        );
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, saved_alignment);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, saved_read);
    }
    pixels
}

/// PAM (netpbm) RGBA, top row first: trivial to write, and ffmpeg reads it.
fn write_pam_flipped(path: &Path, width_px: u32, height_px: u32, bottom_up: &[u8]) -> std::io::Result<()> {
    let row_bytes = width_px as usize * 4;
    assert_eq!(bottom_up.len(), row_bytes * height_px as usize);
    let mut bytes = format!(
        "P7\nWIDTH {width_px}\nHEIGHT {height_px}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n"
    )
    .into_bytes();
    for row in bottom_up.chunks_exact(row_bytes).rev() {
        bytes.extend_from_slice(row);
    }
    std::fs::write(path, bytes)
}
