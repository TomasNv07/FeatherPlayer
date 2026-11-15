//! Minimal runtime-loaded libmpv binding (core API + OpenGL render API).
//! No import library is needed: the DLL / .so is opened with libloading.

use libloading::Library;
use std::ffi::{c_char, c_int, c_ulong, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicPtr, Ordering};

pub type Handle = *mut c_void;

#[repr(C)]
pub struct Event {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

pub const EV_NONE: c_int = 0;
pub const EV_SHUTDOWN: c_int = 1;
pub const EV_END_FILE: c_int = 7;
pub const EV_FILE_LOADED: c_int = 8;

const FORMAT_DOUBLE: c_int = 5;

const RP_INVALID: c_int = 0;
const RP_API_TYPE: c_int = 1;
const RP_OPENGL_INIT_PARAMS: c_int = 2;
const RP_OPENGL_FBO: c_int = 3;
const RP_FLIP_Y: c_int = 4;

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}
type GetProcAddr = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
#[repr(C)]
struct OpenGlInit {
    get_proc_address: Option<GetProcAddr>,
    ctx: *mut c_void,
}
#[repr(C)]
struct OpenGlFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

pub struct Core {
    _lib: Library,
    pub lib_path: PathBuf,
    pub api_version: c_ulong,
    h: AtomicPtr<c_void>,
    render_ctx: AtomicPtr<c_void>,
    /// raw `Box<egui::Context>` used by the update callback (freed in `shutdown`)
    update_ctx: AtomicPtr<c_void>,

    f_create: unsafe extern "C" fn() -> Handle,
    f_initialize: unsafe extern "C" fn(Handle) -> c_int,
    f_set_option_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    f_set_property_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    f_get_property_string: unsafe extern "C" fn(Handle, *const c_char) -> *mut c_char,
    f_get_property: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    f_free: unsafe extern "C" fn(*mut c_void),
    f_command: unsafe extern "C" fn(Handle, *mut *const c_char) -> c_int,
    f_wait_event: unsafe extern "C" fn(Handle, f64) -> *mut Event,
    f_terminate_destroy: unsafe extern "C" fn(Handle),
    f_error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    f_rc_create: unsafe extern "C" fn(*mut *mut c_void, Handle, *mut RenderParam) -> c_int,
    f_rc_render: unsafe extern "C" fn(*mut c_void, *mut RenderParam) -> c_int,
    f_rc_set_update_cb:
        unsafe extern "C" fn(*mut c_void, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void),
    f_rc_update: unsafe extern "C" fn(*mut c_void) -> u64,
    f_rc_free: unsafe extern "C" fn(*mut c_void),
}

// libmpv's client API is thread-safe; the render context is only touched from the GL thread.
unsafe impl Send for Core {}
unsafe impl Sync for Core {}

unsafe extern "C" fn on_update(ctx: *mut c_void) {
    // Called by mpv from an arbitrary thread when a new video frame is ready.
    if !ctx.is_null() {
        let c = &*(ctx as *const eframe::egui::Context);
        c.request_repaint();
    }
}

unsafe extern "C" fn proc_trampoline(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    let f = &*(ctx as *const &dyn Fn(&CStr) -> *const c_void);
    f(CStr::from_ptr(name)) as *mut c_void
}

pub fn default_candidates() -> Vec<PathBuf> {
    let names = ["libmpv-2.dll", "mpv-2.dll", "libmpv.so.2", "libmpv.so.1", "libmpv.dylib"];
    let mut v: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("FEATHER_LIBMPV") {
        v.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.extend(names.iter().map(|n| dir.join(n)));
        }
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        v.push(PathBuf::from(home).join("FeatherPlayer").join("libmpv-2.dll"));
    }
    v.extend(names.iter().map(PathBuf::from));
    v
}

impl Core {
    pub fn load(candidates: &[PathBuf]) -> Result<Core, String> {
        let mut tried = Vec::new();
        for c in candidates {
            match unsafe { Library::new(c) } {
                Ok(lib) => return unsafe { Core::bind(lib, c.clone()) },
                Err(e) => tried.push(format!("  {} -> {e}", c.display())),
            }
        }
        Err(format!(
            "Could not load libmpv. Put libmpv-2.dll next to the exe, in %USERPROFILE%\\FeatherPlayer, \
             or set FEATHER_LIBMPV.\nTried:\n{}",
            tried.join("\n")
        ))
    }

    unsafe fn bind(lib: Library, lib_path: PathBuf) -> Result<Core, String> {
        macro_rules! sym {
            ($name:literal) => {
                *lib.get($name).map_err(|e| {
                    format!("{} is missing {}: {e}", lib_path.display(), String::from_utf8_lossy($name))
                })?
            };
        }
        let api: unsafe extern "C" fn() -> c_ulong = sym!(b"mpv_client_api_version\0");
        let api_version = api();
        Ok(Core {
            f_create: sym!(b"mpv_create\0"),
            f_initialize: sym!(b"mpv_initialize\0"),
            f_set_option_string: sym!(b"mpv_set_option_string\0"),
            f_set_property_string: sym!(b"mpv_set_property_string\0"),
            f_get_property_string: sym!(b"mpv_get_property_string\0"),
            f_get_property: sym!(b"mpv_get_property\0"),
            f_free: sym!(b"mpv_free\0"),
            f_command: sym!(b"mpv_command\0"),
            f_wait_event: sym!(b"mpv_wait_event\0"),
            f_terminate_destroy: sym!(b"mpv_terminate_destroy\0"),
            f_error_string: sym!(b"mpv_error_string\0"),
            f_rc_create: sym!(b"mpv_render_context_create\0"),
            f_rc_render: sym!(b"mpv_render_context_render\0"),
            f_rc_set_update_cb: sym!(b"mpv_render_context_set_update_callback\0"),
            f_rc_update: sym!(b"mpv_render_context_update\0"),
            f_rc_free: sym!(b"mpv_render_context_free\0"),
            h: AtomicPtr::new(std::ptr::null_mut()),
            render_ctx: AtomicPtr::new(std::ptr::null_mut()),
            update_ctx: AtomicPtr::new(std::ptr::null_mut()),
            api_version,
            lib_path,
            _lib: lib,
        })
    }

    fn handle(&self) -> Handle {
        self.h.load(Ordering::SeqCst)
    }

    pub fn err(&self, code: c_int) -> String {
        unsafe { CStr::from_ptr((self.f_error_string)(code)).to_string_lossy().into_owned() }
    }

    /// Create + initialise the mpv instance with the given options.
    pub fn init(&self, options: &[(&str, &str)]) -> Result<(), String> {
        unsafe {
            let h = (self.f_create)();
            if h.is_null() {
                return Err("mpv_create returned null".into());
            }
            for (k, v) in options {
                let (k, v) = (CString::new(*k).unwrap(), CString::new(*v).unwrap());
                let r = (self.f_set_option_string)(h, k.as_ptr(), v.as_ptr());
                if r < 0 {
                    (self.f_terminate_destroy)(h);
                    return Err(format!("option {k:?}={v:?}: {}", self.err(r)));
                }
            }
            let r = (self.f_initialize)(h);
            if r < 0 {
                (self.f_terminate_destroy)(h);
                return Err(format!("mpv_initialize: {}", self.err(r)));
            }
            self.h.store(h, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Must be called on the GL thread with the GL context current.
    pub fn create_render_context(
        &self,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
        egui_ctx: eframe::egui::Context,
    ) -> Result<(), String> {
        unsafe {
            let api = CString::new("opengl").unwrap();
            let gpa_ref: &dyn Fn(&CStr) -> *const c_void = get_proc_address;
            let mut init = OpenGlInit {
                get_proc_address: Some(proc_trampoline),
                ctx: &gpa_ref as *const &dyn Fn(&CStr) -> *const c_void as *mut c_void,
            };
            let mut params = [
                RenderParam { kind: RP_API_TYPE, data: api.as_ptr() as *mut c_void },
                RenderParam { kind: RP_OPENGL_INIT_PARAMS, data: &mut init as *mut _ as *mut c_void },
                RenderParam { kind: RP_INVALID, data: std::ptr::null_mut() },
            ];
            let mut rc: *mut c_void = std::ptr::null_mut();
            let r = (self.f_rc_create)(&mut rc, self.handle(), params.as_mut_ptr());
            if r < 0 || rc.is_null() {
                return Err(format!("mpv_render_context_create: {}", self.err(r)));
            }
            self.render_ctx.store(rc, Ordering::SeqCst);
            let boxed = Box::into_raw(Box::new(egui_ctx)) as *mut c_void;
            self.update_ctx.store(boxed, Ordering::SeqCst);
            (self.f_rc_set_update_cb)(rc, Some(on_update), boxed);
        }
        Ok(())
    }

    /// Render the current frame into `fbo` (w x h). GL thread only.
    pub fn render(&self, fbo: i32, w: i32, h: i32) -> bool {
        let rc = self.render_ctx.load(Ordering::SeqCst);
        if rc.is_null() {
            return false;
        }
        unsafe {
            (self.f_rc_update)(rc);
            let mut f = OpenGlFbo { fbo, w, h, internal_format: 0x8058 /* GL_RGBA8 */ };
            let mut flip: c_int = 1;
            let mut params = [
                RenderParam { kind: RP_OPENGL_FBO, data: &mut f as *mut _ as *mut c_void },
                RenderParam { kind: RP_FLIP_Y, data: &mut flip as *mut _ as *mut c_void },
                RenderParam { kind: RP_INVALID, data: std::ptr::null_mut() },
            ];
            (self.f_rc_render)(rc, params.as_mut_ptr()) >= 0
        }
    }

    /// Free the render context. GL thread, GL context current, before `shutdown`.
    pub fn free_render_context(&self) {
        let rc = self.render_ctx.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !rc.is_null() {
            unsafe {
                (self.f_rc_set_update_cb)(rc, None, std::ptr::null_mut());
                (self.f_rc_free)(rc);
            }
        }
        let u = self.update_ctx.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !u.is_null() {
            unsafe { drop(Box::from_raw(u as *mut eframe::egui::Context)) };
        }
    }

    pub fn shutdown(&self) {
        self.free_render_context();
        let h = self.h.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !h.is_null() {
            unsafe { (self.f_terminate_destroy)(h) };
        }
    }

    pub fn cmd(&self, args: &[&str]) -> Result<(), String> {
        let h = self.handle();
        if h.is_null() {
            return Err("mpv not running".into());
        }
        let owned: Vec<CString> = args.iter().map(|a| CString::new(*a).unwrap()).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let r = unsafe { (self.f_command)(h, ptrs.as_mut_ptr()) };
        if r < 0 { Err(format!("{args:?}: {}", self.err(r))) } else { Ok(()) }
    }

    pub fn set(&self, name: &str, value: &str) -> Result<(), String> {
        let h = self.handle();
        if h.is_null() {
            return Err("mpv not running".into());
        }
        let (n, v) = (CString::new(name).unwrap(), CString::new(value).unwrap());
        let r = unsafe { (self.f_set_property_string)(h, n.as_ptr(), v.as_ptr()) };
        if r < 0 { Err(format!("set {name}={value}: {}", self.err(r))) } else { Ok(()) }
    }

    pub fn get(&self, name: &str) -> Option<String> {
        let h = self.handle();
        if h.is_null() {
            return None;
        }
        let n = CString::new(name).unwrap();
        unsafe {
            let p = (self.f_get_property_string)(h, n.as_ptr());
            if p.is_null() {
                return None;
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            (self.f_free)(p as *mut c_void);
            Some(s)
        }
    }

    pub fn getf(&self, name: &str) -> Option<f64> {
        let h = self.handle();
        if h.is_null() {
            return None;
        }
        let n = CString::new(name).unwrap();
        let mut v: f64 = 0.0;
        let r = unsafe { (self.f_get_property)(h, n.as_ptr(), FORMAT_DOUBLE, &mut v as *mut f64 as *mut c_void) };
        if r < 0 { None } else { Some(v) }
    }

    pub fn flag(&self, name: &str) -> bool {
        self.get(name).map(|s| s == "yes").unwrap_or(false)
    }

    /// Non-blocking event poll; returns the event id (EV_NONE when the queue is empty).
    pub fn poll_event(&self) -> c_int {
        let h = self.handle();
        if h.is_null() {
            return EV_NONE;
        }
        unsafe { (*(self.f_wait_event)(h, 0.0)).event_id }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.shutdown();
    }
}
