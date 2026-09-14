//! Reads the rendered frame back with glReadPixels, to check what is really on
//! screen. `Window::take_snapshot` returned all-zero buffers here, so it can't
//! be trusted (see spikes/slint-size/README.md).

use std::path::Path;

use glow::HasContext;

/// Reads the just-rendered back buffer as RGBA rows, bottom row first.
/// Call from `AfterRendering`, before the buffers are swapped.
pub fn read_back(gl: &glow::Context, width_px: u32, height_px: u32) -> Vec<u8> {
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
pub fn write_pam_flipped(path: &Path, width_px: u32, height_px: u32, bottom_up: &[u8]) -> std::io::Result<()> {
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
