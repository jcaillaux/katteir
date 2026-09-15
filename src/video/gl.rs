//! Stacked-alpha YUV -> RGBA on the GPU.
//!
//! Each decoded frame is 4:2:0 YUV whose top half is the colour picture and
//! whose bottom half carries alpha in the luma plane. The three planes are
//! uploaded as R8 textures; one fragment shader converts BT.709 limited-range
//! YUV to RGB, takes alpha from the bottom half, and renders into an RGBA
//! texture that Slint then draws as a borrowed OpenGL texture.
//!
//! All GL state we touch is saved before and restored after, because we run
//! inside Slint's (femtovg's) own GL context.
//!
//! Ported from `spikes/av1-video`. Raw GL is unsafe FFI, so this is one of the
//! two places where the app allows `unsafe` (CLAUDE.md §4); every block has a
//! SAFETY comment.
#![allow(unsafe_code)]

use std::rc::Rc;

use glow::HasContext;

/// The GL function table for Slint's context. Call it from the rendering
/// notifier's `RenderingSetup`.
pub fn context(graphics_api: &slint::GraphicsAPI<'_>) -> Option<Rc<glow::Context>> {
    let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
        log::error!("the cat window needs Slint's OpenGL renderer");
        return None;
    };
    // SAFETY: Slint makes its GL context current before calling the rendering
    // notifier, and get_proc_address resolves symbols for exactly that context.
    Some(Rc::new(unsafe { glow::Context::from_loader_function_cstr(|name| get_proc_address(name)) }))
}

const VERTEX_SHADER: &str = r"#version 100
attribute vec2 position;
varying vec2 uv; // uv.y == 0 at the top row of the picture
void main() {
    uv = vec2(0.5 * position.x + 0.5, 0.5 * position.y + 0.5);
    gl_Position = vec4(position, 0.0, 1.0);
}";

// BT.709 limited range. Output is straight (not premultiplied) alpha: Slint's
// femtovg renderer imports borrowed textures without ImageFlags::PREMULTIPLIED,
// so femtovg multiplies by alpha itself when it draws them.
const FRAGMENT_SHADER: &str = r"#version 100
precision mediump float;
varying vec2 uv;
uniform sampler2D y_plane;
uniform sampler2D u_plane;
uniform sampler2D v_plane;
void main() {
    vec2 colour_uv = vec2(uv.x, uv.y * 0.5);
    vec2 alpha_uv = vec2(uv.x, 0.5 + uv.y * 0.5);
    float y = texture2D(y_plane, colour_uv).r - 16.0 / 255.0;
    float u = texture2D(u_plane, colour_uv).r - 0.5;
    float v = texture2D(v_plane, colour_uv).r - 0.5;
    float a = clamp((texture2D(y_plane, alpha_uv).r - 16.0 / 255.0) * (255.0 / 219.0), 0.0, 1.0);
    vec3 rgb = vec3(
        1.1644 * y + 1.7927 * v,
        1.1644 * y - 0.2132 * u - 0.5329 * v,
        1.1644 * y + 2.1124 * u);
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), a);
}";

/// Y, U, V.
const PLANE_COUNT: usize = 3;
/// Two output textures: draw into one while Slint may still hold the other.
const TARGET_COUNT: usize = 2;
/// Largest stacked-frame side the video path accepts. Keeps every GL size
/// (strides included, at most twice this) far inside `i32`.
const MAX_SIDE_PX: u32 = 8192;

/// A size as GL's `i32`. Every size passed here is asserted to be at most
/// `2 * MAX_SIDE_PX`.
fn gl_int(value: u32) -> i32 {
    i32::try_from(value).expect("GL sizes are asserted to be <= 2 * MAX_SIDE_PX")
}

/// `glow::TEXTURE0 + unit` for one of the plane texture units.
fn texture_unit(unit: usize) -> u32 {
    assert!(unit < PLANE_COUNT);
    glow::TEXTURE0 + u32::try_from(unit).expect("unit < PLANE_COUNT")
}

struct Target {
    texture: glow::Texture,
    framebuffer: glow::Framebuffer,
}

pub struct GlVideo {
    gl: Rc<glow::Context>,
    program: glow::Program,
    vertex_buffer: glow::Buffer,
    vertex_array: glow::VertexArray,
    planes: [glow::Texture; PLANE_COUNT],
    targets: [Target; TARGET_COUNT],
    next_target: usize,
    stacked_width_px: u32,
    stacked_height_px: u32,
}

/// GL state that `draw_frame` changes, captured so it can be put back.
struct SavedState {
    program: Option<glow::Program>,
    active_texture: u32,
    textures: [Option<glow::Texture>; PLANE_COUNT],
    framebuffer: Option<glow::Framebuffer>,
    vertex_array: Option<glow::VertexArray>,
    array_buffer: Option<glow::Buffer>,
    viewport: [i32; 4],
    blend: bool,
    scissor: bool,
    unpack_alignment: i32,
    unpack_row_length: i32,
}

impl GlVideo {
    pub fn new(gl: Rc<glow::Context>, stacked_width_px: u32, stacked_height_px: u32) -> Result<Self, String> {
        assert!(stacked_width_px > 0 && stacked_width_px.is_multiple_of(2));
        assert!(stacked_height_px > 0 && stacked_height_px.is_multiple_of(4));
        assert!(stacked_width_px <= MAX_SIDE_PX && stacked_height_px <= MAX_SIDE_PX, "frame larger than 8192 px");
        // SAFETY: called from RenderingSetup with Slint's context current; all
        // objects created here are owned by `Self` and deleted in `Drop`.
        unsafe {
            let saved = SavedState::capture(&gl);
            let program = link_program(&gl)?;
            let (vertex_buffer, vertex_array) = create_quad(&gl, program)?;
            let planes = create_planes(&gl, stacked_width_px, stacked_height_px)?;
            let colour_height_px = stacked_height_px / 2;
            let targets = [
                create_target(&gl, stacked_width_px, colour_height_px)?,
                create_target(&gl, stacked_width_px, colour_height_px)?,
            ];
            gl.use_program(Some(program));
            for (unit, name) in [(0, "y_plane"), (1, "u_plane"), (2, "v_plane")] {
                let location = gl.get_uniform_location(program, name).ok_or(format!("no uniform {name}"))?;
                gl.uniform_1_i32(Some(&location), unit);
            }
            saved.restore(&gl);
            Ok(Self {
                gl,
                program,
                vertex_buffer,
                vertex_array,
                planes,
                targets,
                next_target: 0,
                stacked_width_px,
                stacked_height_px,
            })
        }
    }

    /// Size of the stacked frames this was set up for.
    pub fn stacked_size_px(&self) -> (u32, u32) {
        (self.stacked_width_px, self.stacked_height_px)
    }

    /// Uploads `picture`, converts it into the next target and returns that
    /// target as a Slint image. Call only from `BeforeRendering`.
    pub fn draw_frame(&mut self, picture: &dav1d::Picture) -> slint::Image {
        assert_eq!(picture.width(), self.stacked_width_px);
        assert_eq!(picture.height(), self.stacked_height_px);
        assert_eq!(picture.pixel_layout(), dav1d::PixelLayout::I420);
        assert_eq!(picture.bit_depth(), 8);
        let target_index = self.next_target;
        let colour_height_px = self.stacked_height_px / 2;
        // SAFETY: Slint's context is current during BeforeRendering; every
        // object used is owned by `self`; plane slices outlive the upload calls.
        unsafe {
            let saved = SavedState::capture(&self.gl);
            self.upload_planes(picture);
            let gl = &self.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.targets[target_index].framebuffer));
            gl.viewport(0, 0, gl_int(self.stacked_width_px), gl_int(colour_height_px));
            gl.disable(glow::BLEND);
            gl.disable(glow::SCISSOR_TEST);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vertex_array));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            saved.restore(gl);
        }
        self.next_target = (target_index + 1) % TARGET_COUNT;
        let texture_id = self.targets[target_index].texture.0;
        // SAFETY: the texture is a live GL_TEXTURE_2D with RGBA storage owned
        // by `self`, which lives until RenderingTeardown.
        unsafe {
            slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                texture_id,
                (self.stacked_width_px, colour_height_px).into(),
            )
            .build()
        }
    }

    /// # Safety
    /// GL context must be current; caller restores state afterwards.
    unsafe fn upload_planes(&self, picture: &dav1d::Picture) {
        use dav1d::PlanarImageComponent::{U, V, Y};
        let gl = &self.gl;
        let sizes = plane_sizes(self.stacked_width_px, self.stacked_height_px);
        for (unit, component) in [Y, U, V].into_iter().enumerate() {
            let (width_px, height_px) = sizes[unit];
            let stride = picture.stride(component);
            let plane = picture.plane(component);
            assert!(stride >= width_px && stride <= 2 * MAX_SIDE_PX);
            assert!(plane.len() >= (stride * (height_px - 1) + width_px) as usize);
            // SAFETY: forwarded from the caller; `plane` outlives the call.
            unsafe {
                gl.active_texture(texture_unit(unit));
                gl.bind_texture(glow::TEXTURE_2D, Some(self.planes[unit]));
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, gl_int(stride));
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    gl_int(width_px),
                    gl_int(height_px),
                    glow::RED,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&plane[..])),
                );
            }
        }
    }
}

impl Drop for GlVideo {
    fn drop(&mut self) {
        // SAFETY: dropped in RenderingTeardown while the context is still
        // current; these objects were created by us and are not used after.
        unsafe {
            for target in &self.targets {
                self.gl.delete_framebuffer(target.framebuffer);
                self.gl.delete_texture(target.texture);
            }
            for plane in self.planes {
                self.gl.delete_texture(plane);
            }
            self.gl.delete_vertex_array(self.vertex_array);
            self.gl.delete_buffer(self.vertex_buffer);
            self.gl.delete_program(self.program);
        }
    }
}

fn plane_sizes(stacked_width_px: u32, stacked_height_px: u32) -> [(u32, u32); PLANE_COUNT] {
    let luma = (stacked_width_px, stacked_height_px);
    let chroma = (stacked_width_px / 2, stacked_height_px / 2);
    [luma, chroma, chroma]
}

impl SavedState {
    unsafe fn capture(gl: &glow::Context) -> Self {
        // SAFETY: plain state queries on the current context.
        unsafe {
            let active_texture = u32::try_from(gl.get_parameter_i32(glow::ACTIVE_TEXTURE)).unwrap_or(glow::TEXTURE0);
            let mut textures = [None; PLANE_COUNT];
            for (unit, slot) in textures.iter_mut().enumerate() {
                gl.active_texture(texture_unit(unit));
                *slot = gl.get_parameter_texture(glow::TEXTURE_BINDING_2D);
            }
            gl.active_texture(active_texture);
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            Self {
                program: gl.get_parameter_program(glow::CURRENT_PROGRAM),
                active_texture,
                textures,
                framebuffer: gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING),
                vertex_array: gl.get_parameter_vertex_array(glow::VERTEX_ARRAY_BINDING),
                array_buffer: gl.get_parameter_buffer(glow::ARRAY_BUFFER_BINDING),
                viewport,
                blend: gl.is_enabled(glow::BLEND),
                scissor: gl.is_enabled(glow::SCISSOR_TEST),
                unpack_alignment: gl.get_parameter_i32(glow::UNPACK_ALIGNMENT),
                unpack_row_length: gl.get_parameter_i32(glow::UNPACK_ROW_LENGTH),
            }
        }
    }

    unsafe fn restore(&self, gl: &glow::Context) {
        // SAFETY: re-binds objects that were bound when captured.
        unsafe {
            for (unit, texture) in self.textures.iter().enumerate() {
                gl.active_texture(texture_unit(unit));
                gl.bind_texture(glow::TEXTURE_2D, *texture);
            }
            gl.active_texture(self.active_texture);
            gl.use_program(self.program);
            gl.bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
            gl.bind_vertex_array(self.vertex_array);
            gl.bind_buffer(glow::ARRAY_BUFFER, self.array_buffer);
            let [x, y, width, height] = self.viewport;
            gl.viewport(x, y, width, height);
            set_enabled(gl, glow::BLEND, self.blend);
            set_enabled(gl, glow::SCISSOR_TEST, self.scissor);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, self.unpack_alignment);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, self.unpack_row_length);
        }
    }
}

unsafe fn set_enabled(gl: &glow::Context, capability: u32, enabled: bool) {
    // SAFETY: toggling a capability on the current context.
    unsafe {
        if enabled {
            gl.enable(capability);
        } else {
            gl.disable(capability);
        }
    }
}

unsafe fn link_program(gl: &glow::Context) -> Result<glow::Program, String> {
    // SAFETY: object creation on the current context; shaders are detached
    // and deleted once linked.
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = [None; 2];
        for (slot, (kind, source)) in
            [(glow::VERTEX_SHADER, VERTEX_SHADER), (glow::FRAGMENT_SHADER, FRAGMENT_SHADER)].iter().enumerate()
        {
            let shader = gl.create_shader(*kind)?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(gl.get_shader_info_log(shader));
            }
            gl.attach_shader(program, shader);
            shaders[slot] = Some(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(gl.get_program_info_log(program));
        }
        for shader in shaders.into_iter().flatten() {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        Ok(program)
    }
}

unsafe fn create_quad(gl: &glow::Context, program: glow::Program) -> Result<(glow::Buffer, glow::VertexArray), String> {
    const QUAD: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
    // SAFETY: object creation on the current context; QUAD is plain f32 data.
    unsafe {
        let position = gl.get_attrib_location(program, "position").ok_or("no attribute position")?;
        let buffer = gl.create_buffer()?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
        let bytes: Vec<u8> = QUAD.iter().flat_map(|v| v.to_ne_bytes()).collect();
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes, glow::STATIC_DRAW);
        let vertex_array = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(vertex_array));
        gl.enable_vertex_attrib_array(position);
        gl.vertex_attrib_pointer_f32(position, 2, glow::FLOAT, false, 8, 0);
        Ok((buffer, vertex_array))
    }
}

unsafe fn create_planes(gl: &glow::Context, stacked_width_px: u32, stacked_height_px: u32) -> Result<[glow::Texture; PLANE_COUNT], String> {
    let sizes = plane_sizes(stacked_width_px, stacked_height_px);
    let mut planes = [None; PLANE_COUNT];
    for (slot, (width_px, height_px)) in planes.iter_mut().zip(sizes) {
        // SAFETY: allocates an R8 texture with no initial data.
        *slot = Some(unsafe { create_texture(gl, glow::R8, glow::RED, width_px, height_px)? });
    }
    Ok(planes.map(|p| p.expect("every plane created above")))
}

unsafe fn create_target(gl: &glow::Context, width_px: u32, height_px: u32) -> Result<Target, String> {
    // SAFETY: allocates an RGBA texture and a framebuffer that renders into it.
    unsafe {
        let texture = create_texture(gl, glow::RGBA8, glow::RGBA, width_px, height_px)?;
        let framebuffer = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(texture), 0);
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            return Err(format!("framebuffer incomplete: 0x{status:x}"));
        }
        Ok(Target { texture, framebuffer })
    }
}

unsafe fn create_texture(
    gl: &glow::Context,
    internal_format: u32,
    format: u32,
    width_px: u32,
    height_px: u32,
) -> Result<glow::Texture, String> {
    assert!(width_px > 0 && height_px > 0 && width_px <= MAX_SIDE_PX && height_px <= MAX_SIDE_PX);
    // SAFETY: allocates storage only (no pixel data pointer).
    unsafe {
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        for (parameter, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, parameter, gl_int(value));
        }
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            gl_int(internal_format),
            gl_int(width_px),
            gl_int(height_px),
            0,
            format,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        Ok(texture)
    }
}
