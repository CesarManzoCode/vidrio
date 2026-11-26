//! The X11 window (vault/CONTRACT.md §3.6, D-14) over libxcb with its
//! own declarations ([`xcb`]): a window at (0, 0) of the viewport size
//! with background `None` (the server never paints over a frame) and
//! `WM_NAME`, framebuffers delivered with `xcb_shm_put_image` from a
//! MIT-SHM segment (`shmget`/`shmat`), or with `xcb_put_image` in strips
//! when the server has no SHM. The connection's descriptor joins the
//! event loop through `poll`.
//!
//! [`run`] drives a top-level browsing context: exposure re-presents the
//! last frame, configuration resizes the viewport, the keyboard goes
//! through the X keyboard map ([`keymap`]), buttons and motion through
//! hit testing into DOM events, the wheel scrolls, and closing the window
//! ends the run. [`Renderer`] is the rendering hook: when the document
//! changed it paints with the same [`DocumentPainter`] as WebDriver
//! screenshots, and the first frame waits for the document's `load`
//! event (or one second) so that a static page never shows an
//! incomplete frame.

pub mod keymap;
pub mod xcb;

use std::cell::{Cell, RefCell};
use std::ffi::{c_int, c_void};
use std::ptr;
use std::rc::Rc;
use std::time::{Duration, Instant};

use vjs::Agent;

use crate::browser::{Browser, ContextId, RenderingHook};
use crate::dom::{DocRef, NodeId};
use crate::paint::{DocumentPainter, Framebuffer, PaintOptions, Painter};
use crate::webdriver::input::{self, InputState};
use keymap::Keymap;
use xcb::*;

/// How long the first frame waits for the `load` event.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(1);

/// How far one notch of the wheel scrolls, in CSS pixels.
const WHEEL_STEP: f64 = 48.0;

/// What happened in the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowEvent {
    /// A part of the window must be shown again.
    Expose,
    /// The window has a new size.
    Resize(u32, u32),
    /// The pointer moved to (`x`, `y`).
    Motion { x: i16, y: i16 },
    /// An X button (1 left, 2 middle, 3 right, 4–7 the wheel) went down
    /// or up at (`x`, `y`).
    Button {
        down: bool,
        button: u8,
        x: i16,
        y: i16,
    },
    /// A key went down or up; `state` is the modifier mask.
    Key { down: bool, keycode: u8, state: u16 },
    /// The window manager asked to close the window, or it is gone.
    Close,
}

/// A MIT-SHM segment attached to the server.
struct Shm {
    addr: *mut u8,
    size: usize,
    seg: u32,
}

/// An open X11 window.
pub struct X11Window {
    conn: *mut xcb_connection_t,
    window: u32,
    gc: u32,
    depth: u8,
    size: (u32, u32),
    wm_protocols: u32,
    wm_delete: u32,
    keymap: Keymap,
    /// False once SHM turned out unavailable.
    shm_ok: bool,
    shm: Option<Shm>,
    title: String,
}

fn u16_of(v: u32) -> u16 {
    v.min(u32::from(u16::MAX)) as u16
}

fn read_u16(b: &[u8], at: usize) -> u16 {
    u16::from_ne_bytes([b[at], b[at + 1]])
}

fn read_i16(b: &[u8], at: usize) -> i16 {
    read_u16(b, at) as i16
}

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl X11Window {
    /// Connects to the display of `$DISPLAY` and maps a window of
    /// `width`×`height` at (0, 0) titled `title`.
    pub fn open(width: u32, height: u32, title: &str) -> Result<X11Window, String> {
        // SAFETY: plain libxcb calls on the connection this function
        // owns; every reply is checked for null and freed once read.
        unsafe {
            let mut screen_num: c_int = 0;
            let conn = xcb_connect(ptr::null(), &mut screen_num);
            if conn.is_null() || xcb_connection_has_error(conn) != 0 {
                if !conn.is_null() {
                    xcb_disconnect(conn);
                }
                return Err("cannot connect to the X server (is DISPLAY set?)".to_string());
            }
            let setup = xcb_get_setup(conn);
            let mut it = xcb_setup_roots_iterator(setup);
            for _ in 0..screen_num {
                xcb_screen_next(&mut it);
            }
            if it.data.is_null() {
                xcb_disconnect(conn);
                return Err("the X server has no such screen".to_string());
            }
            let screen = &*it.data;
            let depth = screen.root_depth;
            if !(depth == 24 || depth == 32) || (*setup).image_byte_order != 0 {
                crate::log::info(&format!(
                    "window: root depth {depth}, byte order {}: colors assume 32-bit BGRX",
                    (*setup).image_byte_order
                ));
            }
            let window = xcb_generate_id(conn);
            let mask = XCB_EVENT_MASK_EXPOSURE
                | XCB_EVENT_MASK_STRUCTURE_NOTIFY
                | XCB_EVENT_MASK_KEY_PRESS
                | XCB_EVENT_MASK_KEY_RELEASE
                | XCB_EVENT_MASK_BUTTON_PRESS
                | XCB_EVENT_MASK_BUTTON_RELEASE
                | XCB_EVENT_MASK_POINTER_MOTION;
            // Values in the order of their mask bits.
            let values = [XCB_BACK_PIXMAP_NONE, mask];
            xcb_create_window(
                conn,
                depth,
                window,
                screen.root,
                0,
                0,
                u16_of(width),
                u16_of(height),
                0,
                XCB_WINDOW_CLASS_INPUT_OUTPUT,
                screen.root_visual,
                XCB_CW_BACK_PIXMAP | XCB_CW_EVENT_MASK,
                values.as_ptr() as *const c_void,
            );
            let gc = xcb_generate_id(conn);
            xcb_create_gc(conn, gc, window, 0, ptr::null());
            let wm_protocols = intern(conn, "WM_PROTOCOLS");
            let wm_delete = intern(conn, "WM_DELETE_WINDOW");
            if wm_protocols != 0 && wm_delete != 0 {
                xcb_change_property(
                    conn,
                    XCB_PROP_MODE_REPLACE,
                    window,
                    wm_protocols,
                    XCB_ATOM_ATOM,
                    32,
                    1,
                    &wm_delete as *const u32 as *const c_void,
                );
            }
            let keymap = read_keymap(conn, (*setup).min_keycode, (*setup).max_keycode);
            let shm_ok = has_shm(conn);
            let mut w = X11Window {
                conn,
                window,
                gc,
                depth,
                size: (width, height),
                wm_protocols,
                wm_delete,
                keymap,
                shm_ok,
                shm: None,
                title: String::new(),
            };
            w.set_title(title);
            xcb_map_window(conn, window);
            xcb_flush(conn);
            Ok(w)
        }
    }

    /// Sets `WM_NAME`.
    pub fn set_title(&mut self, title: &str) {
        if self.title == title {
            return;
        }
        self.title = title.to_string();
        // Latin-1 (`STRING`): other characters become `?`.
        let bytes: Vec<u8> = title
            .chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .collect();
        // SAFETY: the property data outlives the call.
        unsafe {
            xcb_change_property(
                self.conn,
                XCB_PROP_MODE_REPLACE,
                self.window,
                XCB_ATOM_WM_NAME,
                XCB_ATOM_STRING,
                8,
                bytes.len() as u32,
                bytes.as_ptr() as *const c_void,
            );
            xcb_flush(self.conn);
        }
    }

    /// The size the window was last configured to.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Whether the connection broke.
    pub fn has_error(&self) -> bool {
        // SAFETY: the connection is open.
        unsafe { xcb_connection_has_error(self.conn) != 0 }
    }

    /// The next queued event, without blocking.
    pub fn poll_event(&mut self) -> Option<WindowEvent> {
        loop {
            // SAFETY: a non-null event is a malloc'd block of at least
            // 36 bytes that this function frees.
            let bytes = unsafe {
                let e = xcb_poll_for_event(self.conn);
                if e.is_null() {
                    return None;
                }
                let b = (*e).bytes;
                free(e as *mut c_void);
                b
            };
            if let Some(ev) = self.decode(&bytes) {
                return Some(ev);
            }
        }
    }

    fn decode(&mut self, b: &[u8; 32]) -> Option<WindowEvent> {
        Some(match b[0] & 0x7f {
            XCB_EXPOSE if read_u16(b, 16) == 0 => WindowEvent::Expose,
            XCB_CONFIGURE_NOTIFY => {
                let size = (u32::from(read_u16(b, 20)), u32::from(read_u16(b, 22)));
                if size == self.size {
                    return None;
                }
                self.size = size;
                WindowEvent::Resize(size.0, size.1)
            }
            XCB_MOTION_NOTIFY => WindowEvent::Motion {
                x: read_i16(b, 24),
                y: read_i16(b, 26),
            },
            t @ (XCB_BUTTON_PRESS | XCB_BUTTON_RELEASE) => WindowEvent::Button {
                down: t == XCB_BUTTON_PRESS,
                button: b[1],
                x: read_i16(b, 24),
                y: read_i16(b, 26),
            },
            t @ (XCB_KEY_PRESS | XCB_KEY_RELEASE) => WindowEvent::Key {
                down: t == XCB_KEY_PRESS,
                keycode: b[1],
                state: read_u16(b, 28),
            },
            XCB_CLIENT_MESSAGE
                if read_u32(b, 8) == self.wm_protocols && read_u32(b, 12) == self.wm_delete =>
            {
                WindowEvent::Close
            }
            XCB_DESTROY_NOTIFY => WindowEvent::Close,
            _ => return None,
        })
    }

    /// The raw key value of a key event.
    pub fn raw_key(&self, keycode: u8, state: u16) -> Option<char> {
        keymap::raw_key(self.keymap.keysym(keycode, state))
    }

    /// Sleeps until the connection has something to read or `timeout`
    /// passes (`None`: no limit).
    pub fn wait(&self, timeout: Option<Duration>) {
        // SAFETY: the descriptor belongs to the open connection.
        unsafe {
            xcb_flush(self.conn);
            let mut fds = pollfd {
                fd: xcb_get_file_descriptor(self.conn),
                events: POLLIN,
                revents: 0,
            };
            let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as c_int);
            poll(&mut fds, 1, ms);
        }
    }

    /// Shows `fb` at the window's origin: the framebuffer composited over
    /// white as 32-bit BGRX pixels.
    pub fn present(&mut self, fb: &Framebuffer) {
        let (w, h) = (fb.width, fb.height);
        if w == 0 || h == 0 {
            return;
        }
        let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
        for p in fb.to_rgb().chunks_exact(3) {
            pixels.extend_from_slice(&[p[2], p[1], p[0], 0]);
        }
        if self.shm_ok && self.present_shm(&pixels, w, h) {
            return;
        }
        self.present_put_image(&pixels, w, h);
    }

    /// Delivers `pixels` through the SHM segment (grown as needed); false
    /// if SHM is not usable.
    fn present_shm(&mut self, pixels: &[u8], w: u32, h: u32) -> bool {
        if self.shm.as_ref().is_none_or(|s| s.size < pixels.len()) {
            self.drop_shm();
            match attach_shm(self.conn, pixels.len()) {
                Some(s) => self.shm = Some(s),
                None => {
                    crate::log::info("window: MIT-SHM is not usable, using xcb_put_image");
                    self.shm_ok = false;
                    return false;
                }
            }
        }
        let Some(shm) = &self.shm else {
            return false;
        };
        // SAFETY: the segment holds at least `pixels.len()` bytes, and
        // the round trip after the request means the server is done
        // reading it before it is written again.
        unsafe {
            ptr::copy_nonoverlapping(pixels.as_ptr(), shm.addr, pixels.len());
            xcb_shm_put_image(
                self.conn,
                self.window,
                self.gc,
                u16_of(w),
                u16_of(h),
                0,
                0,
                u16_of(w),
                u16_of(h),
                0,
                0,
                self.depth,
                XCB_IMAGE_FORMAT_Z_PIXMAP,
                0,
                shm.seg,
                0,
            );
            self.sync();
        }
        true
    }

    /// Delivers `pixels` with `xcb_put_image`, in strips of rows that fit
    /// the maximum request length.
    fn present_put_image(&mut self, pixels: &[u8], w: u32, h: u32) {
        let stride = w as usize * 4;
        // SAFETY: each strip points into `pixels` with its exact length.
        unsafe {
            let max_bytes = xcb_get_maximum_request_length(self.conn) as usize * 4;
            let rows = (max_bytes.saturating_sub(64) / stride).max(1);
            let mut y = 0usize;
            while y < h as usize {
                let n = rows.min(h as usize - y);
                let strip = &pixels[y * stride..(y + n) * stride];
                xcb_put_image(
                    self.conn,
                    XCB_IMAGE_FORMAT_Z_PIXMAP,
                    self.window,
                    self.gc,
                    u16_of(w),
                    n as u16,
                    0,
                    y.min(i16::MAX as usize) as i16,
                    0,
                    self.depth,
                    strip.len() as u32,
                    strip.as_ptr(),
                );
                y += n;
            }
            xcb_flush(self.conn);
        }
    }

    /// A round trip: every request sent before has been processed.
    fn sync(&self) {
        // SAFETY: the reply is freed once received.
        unsafe {
            let cookie = xcb_get_input_focus(self.conn);
            let r = xcb_get_input_focus_reply(self.conn, cookie, ptr::null_mut());
            if !r.is_null() {
                free(r as *mut c_void);
            }
        }
    }

    fn drop_shm(&mut self) {
        if let Some(s) = self.shm.take() {
            // SAFETY: the segment was attached by `attach_shm`.
            unsafe {
                xcb_shm_detach(self.conn, s.seg);
                self.sync();
                shmdt(s.addr as *const c_void);
            }
        }
    }
}

impl Drop for X11Window {
    fn drop(&mut self) {
        self.drop_shm();
        // SAFETY: the connection is not used after this.
        unsafe { xcb_disconnect(self.conn) };
    }
}

/// The atom named `name` (0 if the server refuses).
///
/// # Safety
/// `conn` must be an open connection.
unsafe fn intern(conn: *mut xcb_connection_t, name: &str) -> u32 {
    // SAFETY: the name outlives the request; the reply is freed.
    unsafe {
        let cookie = xcb_intern_atom(conn, 0, name.len() as u16, name.as_ptr() as *const _);
        let r = xcb_intern_atom_reply(conn, cookie, ptr::null_mut());
        if r.is_null() {
            return 0;
        }
        let atom = (*r).atom;
        free(r as *mut c_void);
        atom
    }
}

/// The keyboard map of keycodes `min..=max`.
///
/// # Safety
/// `conn` must be an open connection.
unsafe fn read_keymap(conn: *mut xcb_connection_t, min: u8, max: u8) -> Keymap {
    let count = max.saturating_sub(min).saturating_add(1);
    // SAFETY: the keysyms are copied out of the reply before it is freed.
    unsafe {
        let cookie = xcb_get_keyboard_mapping(conn, min, count);
        let r = xcb_get_keyboard_mapping_reply(conn, cookie, ptr::null_mut());
        if r.is_null() {
            return Keymap {
                min_keycode: min,
                per_keycode: 0,
                keysyms: Vec::new(),
            };
        }
        let len = xcb_get_keyboard_mapping_keysyms_length(r).max(0) as usize;
        let syms = xcb_get_keyboard_mapping_keysyms(r);
        let keysyms = std::slice::from_raw_parts(syms, len).to_vec();
        let per_keycode = usize::from((*r).keysyms_per_keycode);
        free(r as *mut c_void);
        Keymap {
            min_keycode: min,
            per_keycode,
            keysyms,
        }
    }
}

/// Whether the server has the MIT-SHM extension.
///
/// # Safety
/// `conn` must be an open connection.
unsafe fn has_shm(conn: *mut xcb_connection_t) -> bool {
    // SAFETY: the reply is freed.
    unsafe {
        let cookie = xcb_shm_query_version(conn);
        let r = xcb_shm_query_version_reply(conn, cookie, ptr::null_mut());
        if r.is_null() {
            return false;
        }
        free(r as *mut c_void);
        true
    }
}

/// A new System V segment of `size` bytes attached here and on the
/// server; it is marked for removal at once, so it goes away with the
/// last detach even if vidrio dies.
fn attach_shm(conn: *mut xcb_connection_t, size: usize) -> Option<Shm> {
    // SAFETY: glibc's System V calls; the address is checked before use
    // and the segment is released on every failure path.
    unsafe {
        let id = shmget(IPC_PRIVATE, size, IPC_CREAT | 0o600);
        if id < 0 {
            return None;
        }
        let addr = shmat(id, ptr::null(), 0);
        if addr as isize == -1 {
            shmctl(id, IPC_RMID, ptr::null_mut());
            return None;
        }
        let seg = xcb_generate_id(conn);
        let err = xcb_request_check(conn, xcb_shm_attach_checked(conn, seg, id as u32, 1));
        shmctl(id, IPC_RMID, ptr::null_mut());
        if !err.is_null() {
            free(err as *mut c_void);
            shmdt(addr);
            return None;
        }
        Some(Shm {
            addr: addr as *mut u8,
            size,
            seg,
        })
    }
}

// ----- the rendering hook -------------------------------------------------------------

/// The rendering hook of the window: paints the active document of its
/// top-level browsing context when its layout or scroll positions
/// changed, once the first frame may show.
pub struct Renderer {
    ctx: ContextId,
    start: Instant,
    painted: RefCell<Option<(usize, crate::view::LayoutKey, u64)>>,
    frame: RefCell<Option<Framebuffer>>,
    presented: Cell<bool>,
}

impl Renderer {
    pub fn new(ctx: ContextId) -> Renderer {
        Renderer {
            ctx,
            start: Instant::now(),
            painted: RefCell::new(None),
            frame: RefCell::new(None),
            presented: Cell::new(false),
        }
    }

    fn is_top_document(&self, doc: &DocRef) -> bool {
        crate::browser::ua()
            .and_then(|ua| ua.context(self.ctx))
            .and_then(|c| c.active_document())
            .is_some_and(|d| Rc::ptr_eq(&d, doc))
    }

    /// The first frame waits for the `load` event of a document that is
    /// not the initial `about:blank`, or for [`FIRST_FRAME_WAIT`].
    fn may_show(&self, doc: &DocRef) -> bool {
        if self.presented.get() || self.start.elapsed() >= FIRST_FRAME_WAIT {
            return true;
        }
        crate::browser::ua()
            .and_then(|ua| ua.doc_info(doc))
            .is_some_and(|i| i.completely_loaded.get() && !i.is_initial_about_blank.get())
    }

    /// How long until the first frame may show regardless of `load`.
    pub fn first_frame_deadline(&self) -> Option<Duration> {
        let elapsed = self.start.elapsed();
        if self.presented.get() || elapsed >= FIRST_FRAME_WAIT {
            return None;
        }
        Some(FIRST_FRAME_WAIT - elapsed)
    }

    /// The frame painted since the last call, if any.
    pub fn take_frame(&self) -> Option<Framebuffer> {
        let f = self.frame.borrow_mut().take();
        if f.is_some() {
            self.presented.set(true);
        }
        f
    }
}

impl RenderingHook for Renderer {
    fn needs_update(&self, doc: &DocRef) -> bool {
        self.is_top_document(doc)
            && self.may_show(doc)
            && *self.painted.borrow() != Some(crate::view::render_key(doc))
    }

    fn update_rendering(&self, agent: &mut Agent, doc: &DocRef) {
        if !self.needs_update(doc) {
            return;
        }
        let painter = DocumentPainter {
            options: PaintOptions {
                viewport: crate::view::viewport_of(doc),
                ..PaintOptions::default()
            },
        };
        let fb = painter.paint(agent, doc);
        *self.painted.borrow_mut() = Some(crate::view::render_key(doc));
        *self.frame.borrow_mut() = Some(fb);
    }
}

// ----- the run loop -------------------------------------------------------------------

/// Scrolls for a wheel turn over `target` that was not canceled: the
/// nearest scroll container at or above it that can move that way, else
/// the viewport.
fn wheel_scroll(doc: &DocRef, target: NodeId, dx: f64, dy: f64) {
    let mut chain = Vec::new();
    {
        let d = doc.borrow();
        let mut n = Some(target);
        while let Some(x) = n {
            if d.is_element(x) {
                chain.push(crate::view::scroll_target(&d, x));
            }
            n = d.parent(x);
        }
    }
    chain.push(NodeId::DOCUMENT);
    if let Some(&s) = chain
        .iter()
        .find(|&&s| crate::view::can_scroll(doc, s, dx, dy))
    {
        let (x, y) = crate::view::scroll_position(doc, s);
        crate::view::scroll_to(doc, s, x + dx, y + dy);
    }
}

/// The title the window shows for `doc`.
fn title_of(doc: &DocRef) -> String {
    let d = doc.borrow();
    let title = d
        .inclusive_descendants(NodeId::DOCUMENT)
        .into_iter()
        .find(|&n| d.is_html_element(n, "title"))
        .and_then(|t| d.text_content(t))
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    if title.is_empty() {
        "vidrio".to_string()
    } else {
        format!("{title} — vidrio")
    }
}

/// Opens the window for top-level browsing context `ctx` and runs the
/// event loop with it until the window closes.
pub fn run(browser: &mut Browser, ctx: ContextId) -> Result<(), String> {
    let (w, h) = crate::view::viewport();
    let mut win = X11Window::open(w, h, "vidrio")?;
    let renderer = Rc::new(Renderer::new(ctx));
    browser.set_rendering_hook(renderer.clone());
    let mut input = InputState::default();
    let mut last: Option<Framebuffer> = None;
    loop {
        let mut busy = false;
        let mut expose = false;
        while let Some(ev) = win.poll_event() {
            busy = true;
            let doc = browser.document(ctx);
            match ev {
                WindowEvent::Close => return Ok(()),
                WindowEvent::Expose => expose = true,
                WindowEvent::Resize(w, h) => crate::view::set_viewport(w, h),
                WindowEvent::Motion { x, y } => {
                    if let Some(doc) = doc {
                        input::window_pointer_move(
                            browser,
                            &mut input,
                            &doc,
                            f64::from(x),
                            f64::from(y),
                        );
                    }
                }
                WindowEvent::Button { down, button, x, y } => {
                    let Some(doc) = doc else {
                        continue;
                    };
                    let (x, y) = (f64::from(x), f64::from(y));
                    input::window_pointer_move(browser, &mut input, &doc, x, y);
                    match button {
                        1..=3 => {
                            let b = u16::from(button - 1);
                            input::window_button(browser, &mut input, &doc, b, down);
                        }
                        4..=7 if down => {
                            let (dx, dy) = match button {
                                4 => (0.0, -WHEEL_STEP),
                                5 => (0.0, WHEEL_STEP),
                                6 => (-WHEEL_STEP, 0.0),
                                _ => (WHEEL_STEP, 0.0),
                            };
                            if let Some(t) = input::window_wheel(browser, &mut input, &doc, dx, dy)
                            {
                                wheel_scroll(&doc, t, dx, dy);
                            }
                        }
                        _ => {}
                    }
                }
                WindowEvent::Key {
                    down,
                    keycode,
                    state,
                } => {
                    if let (Some(doc), Some(c)) = (doc, win.raw_key(keycode, state)) {
                        input::window_key(browser, &mut input, &doc, c, down);
                    }
                }
            }
        }
        if win.has_error() {
            return Err("the X connection broke".to_string());
        }
        for _ in 0..64 {
            if !browser.spin_once() {
                break;
            }
            busy = true;
        }
        if let Some(fb) = renderer.take_frame() {
            win.present(&fb);
            last = Some(fb);
            if let Some(doc) = browser.document(ctx) {
                win.set_title(&title_of(&doc));
            }
        } else if expose {
            if let Some(fb) = &last {
                win.present(fb);
            }
        }
        if busy {
            continue;
        }
        let mut timeout = crate::browser::event_loop::idle_timeout(&browser.agent);
        if let Some(d) = renderer.first_frame_deadline() {
            timeout = Some(timeout.map_or(d, |t| t.min(d)));
        }
        win.wait(timeout);
    }
}
