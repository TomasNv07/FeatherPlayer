//! GL side: mpv renders into an off-screen texture, which is blitted into the video rectangle.
//! (mpv can only render at the origin of a framebuffer, so it cannot draw straight into a
//! sub-rectangle of the window.)

use crate::mpv;
use eframe::egui::{self, Rect};
use eframe::{egui_glow, glow};
use glow::HasContext;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct VideoGl {
    fbo: Option<glow::Framebuffer>,
    tex: Option<glow::Texture>,
    w: i32,
    h: i32,
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
            st.ensure(gl, w, h);
            let Some(fbo) = st.fbo else { return };
            if core.render(fbo.0.get() as i32, w, h) {
                unsafe {
                    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, st.fbo);
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
                    gl.blit_framebuffer(
                        0,
                        0,
                        w,
                        h,
                        vp.left_px,
                        vp.from_bottom_px,
                        vp.left_px + w,
                        vp.from_bottom_px + h,
                        glow::COLOR_BUFFER_BIT,
                        glow::NEAREST,
                    );
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                }
            }
        })),
    }
}
