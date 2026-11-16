//! GL side: mpv renders into an off-screen texture, which is blitted into the video rectangle.
//! (mpv can only render at the origin of a framebuffer, so it cannot draw straight into a
//! sub-rectangle of the window.)

use crate::mpv;
use eframe::egui::{self, Rect};
use eframe::{egui_glow, glow};
use glow::HasContext;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// While the video rectangle keeps changing size (window resize), mpv keeps rendering at the
/// old size and that picture is scaled into the new rectangle. Re-allocating the texture and
/// making mpv rebuild its scaler buffers for every new size is what made resizing lag.
/// Once the size has been stable this long, it renders at the real size again.
pub const RESIZE_SETTLE: Duration = Duration::from_millis(150);

#[derive(Default)]
pub struct VideoGl {
    fbo: Option<glow::Framebuffer>,
    tex: Option<glow::Texture>,
    w: i32,
    h: i32,
    /// Latest requested size and since when (resize debounce).
    want: (i32, i32),
    want_since: Option<Instant>,
}

impl VideoGl {
    fn ensure(&mut self, gl: &glow::Context, w: i32, h: i32) {
        unsafe {
            if self.fbo.is_none() {
                self.fbo = gl.create_framebuffer().ok();
                self.tex = gl.create_texture().ok();
                self.w = 0;
                self.h = 0;
            }
            if (self.w, self.h) != (w, h) {
                gl.bind_texture(glow::TEXTURE_2D, self.tex);
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    w,
                    h,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
                gl.bind_framebuffer(glow::FRAMEBUFFER, self.fbo);
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, self.tex, 0);
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                gl.bind_texture(glow::TEXTURE_2D, None);
                self.w = w;
                self.h = h;
            }
        }
    }

    pub fn destroy(&mut self, gl: &glow::Context) {
        unsafe {
            if let Some(f) = self.fbo.take() {
                gl.delete_framebuffer(f);
            }
            if let Some(t) = self.tex.take() {
                gl.delete_texture(t);
            }
        }
    }
}

/// Render a waiting mpv frame into the player's own texture without showing it (a player
/// that is loading or preloading off screen; see `Core::render_if_wanted`). Keeps the
/// texture's size, so a swapped-in preload doesn't have to re-allocate. GL thread.
pub fn render_offscreen(core: &mpv::Core, vgl: &Mutex<VideoGl>, gl: &glow::Context, w: i32, h: i32) {
    let mut st = vgl.lock().unwrap();
    if st.w <= 0 || st.h <= 0 {
        st.ensure(gl, w.max(16), h.max(16));
    }
    let (rw, rh) = (st.w, st.h);
    if let Some(fbo) = st.fbo {
        core.render_if_wanted(fbo.0.get() as i32, rw, rh);
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, None) };
    }
}

/// Paint callback that renders the current mpv frame into `rect`.
pub fn video_callback(core: Arc<mpv::Core>, vgl: Arc<Mutex<VideoGl>>, rect: Rect) -> egui::PaintCallback {
    egui::PaintCallback {
        rect,
        callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
            let vp = info.viewport_in_pixels();
            let (w, h) = (vp.width_px, vp.height_px);
            if w <= 0 || h <= 0 {
                return;
            }
            let gl: &glow::Context = painter.gl().as_ref();
            let mut st = vgl.lock().unwrap();
            if st.want != (w, h) {
                st.want = (w, h);
                st.want_since = Some(Instant::now());
            }
            let settled = st.want_since.is_none_or(|t| t.elapsed() >= RESIZE_SETTLE);
            // keep the current texture while resizing (none yet: allocate right away)
            if settled || st.w <= 0 || st.h <= 0 {
                st.ensure(gl, w, h);
            }
            let (rw, rh) = (st.w, st.h);
            let Some(fbo) = st.fbo else { return };
            if core.render(fbo.0.get() as i32, rw, rh) {
                // same size: 1:1. While resizing: scale the old picture evenly and centre it
                // (stretching it to the new rect distorted the video's aspect ratio)
                let (dx, dy, dw, dh, filter) = if (rw, rh) == (w, h) {
                    (0, 0, w, h, glow::NEAREST)
                } else {
                    let s = (w as f32 / rw as f32).min(h as f32 / rh as f32);
                    let (dw, dh) = ((rw as f32 * s).round() as i32, (rh as f32 * s).round() as i32);
                    ((w - dw) / 2, (h - dh) / 2, dw, dh, glow::LINEAR)
                };
                unsafe {
                    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, st.fbo);
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
                    gl.blit_framebuffer(
                        0,
                        0,
                        rw,
                        rh,
                        vp.left_px + dx,
                        vp.from_bottom_px + dy,
                        vp.left_px + dx + dw,
                        vp.from_bottom_px + dy + dh,
                        glow::COLOR_BUFFER_BIT,
                        filter,
                    );
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                }
            }
        })),
    }
}
