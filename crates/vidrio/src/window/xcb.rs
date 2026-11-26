//! The part of libxcb, libxcb-shm and glibc's System V shared memory
//! that the window uses, declared by hand (vault/CONTRACT.md §2: no
//! bindings crate). Layouts follow `xcb/xproto.h`, `xcb/shm.h` and
//! `sys/shm.h`; only the fields read here are spelled out, the rest of
//! each reply stays opaque behind them.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_uint, c_void};

#[repr(C)]
pub struct xcb_connection_t {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_void_cookie_t {
    pub sequence: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_cookie_t {
    pub sequence: c_uint,
}

#[repr(C)]
pub struct xcb_generic_error_t {
    pub response_type: u8,
    pub error_code: u8,
    pub sequence: u16,
    pub resource_id: u32,
    pub minor_code: u16,
    pub major_code: u8,
    pub pad0: u8,
    pub pad: [u32; 5],
    pub full_sequence: u32,
}

/// `xcb_generic_event_t` and every event: 32 bytes and the full
/// sequence number.
#[repr(C)]
pub struct xcb_generic_event_t {
    pub bytes: [u8; 32],
    pub full_sequence: u32,
}

#[repr(C)]
pub struct xcb_setup_t {
    pub status: u8,
    pub pad0: u8,
    pub protocol_major_version: u16,
    pub protocol_minor_version: u16,
    pub length: u16,
    pub release_number: u32,
    pub resource_id_base: u32,
    pub resource_id_mask: u32,
    pub motion_buffer_size: u32,
    pub vendor_len: u16,
    pub maximum_request_length: u16,
    pub roots_len: u8,
    pub pixmap_formats_len: u8,
    pub image_byte_order: u8,
    pub bitmap_format_bit_order: u8,
    pub bitmap_format_scanline_unit: u8,
    pub bitmap_format_scanline_pad: u8,
    pub min_keycode: u8,
    pub max_keycode: u8,
    pub pad1: [u8; 4],
}

#[repr(C)]
pub struct xcb_screen_t {
    pub root: u32,
    pub default_colormap: u32,
    pub white_pixel: u32,
    pub black_pixel: u32,
    pub current_input_masks: u32,
    pub width_in_pixels: u16,
    pub height_in_pixels: u16,
    pub width_in_millimeters: u16,
    pub height_in_millimeters: u16,
    pub min_installed_maps: u16,
    pub max_installed_maps: u16,
    pub root_visual: u32,
    pub backing_stores: u8,
    pub save_unders: u8,
    pub root_depth: u8,
    pub allowed_depths_len: u8,
}

#[repr(C)]
pub struct xcb_screen_iterator_t {
    pub data: *mut xcb_screen_t,
    pub rem: c_int,
    pub index: c_int,
}

#[repr(C)]
pub struct xcb_intern_atom_reply_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub length: u32,
    pub atom: u32,
}

#[repr(C)]
pub struct xcb_get_keyboard_mapping_reply_t {
    pub response_type: u8,
    pub keysyms_per_keycode: u8,
    pub sequence: u16,
    pub length: u32,
    pub pad0: [u8; 24],
}

#[repr(C)]
pub struct xcb_shm_query_version_reply_t {
    pub response_type: u8,
    pub shared_pixmaps: u8,
    pub sequence: u16,
    pub length: u32,
    pub major_version: u16,
    pub minor_version: u16,
    pub uid: u16,
    pub gid: u16,
    pub pixmap_format: u8,
    pub pad0: [u8; 15],
}

#[repr(C)]
pub struct xcb_get_input_focus_reply_t {
    pub response_type: u8,
    pub revert_to: u8,
    pub sequence: u16,
    pub length: u32,
    pub focus: u32,
}

// ----- constants (xproto.h, shm.h, sys/ipc.h) ----------------------------------------

pub const XCB_WINDOW_CLASS_INPUT_OUTPUT: u16 = 1;
pub const XCB_CW_BACK_PIXMAP: u32 = 1;
pub const XCB_CW_EVENT_MASK: u32 = 2048;
pub const XCB_BACK_PIXMAP_NONE: u32 = 0;

pub const XCB_EVENT_MASK_KEY_PRESS: u32 = 1;
pub const XCB_EVENT_MASK_KEY_RELEASE: u32 = 2;
pub const XCB_EVENT_MASK_BUTTON_PRESS: u32 = 4;
pub const XCB_EVENT_MASK_BUTTON_RELEASE: u32 = 8;
pub const XCB_EVENT_MASK_POINTER_MOTION: u32 = 64;
pub const XCB_EVENT_MASK_EXPOSURE: u32 = 32768;
pub const XCB_EVENT_MASK_STRUCTURE_NOTIFY: u32 = 131072;

pub const XCB_KEY_PRESS: u8 = 2;
pub const XCB_KEY_RELEASE: u8 = 3;
pub const XCB_BUTTON_PRESS: u8 = 4;
pub const XCB_BUTTON_RELEASE: u8 = 5;
pub const XCB_MOTION_NOTIFY: u8 = 6;
pub const XCB_EXPOSE: u8 = 12;
pub const XCB_DESTROY_NOTIFY: u8 = 17;
pub const XCB_CONFIGURE_NOTIFY: u8 = 22;
pub const XCB_CLIENT_MESSAGE: u8 = 33;

pub const XCB_PROP_MODE_REPLACE: u8 = 0;
pub const XCB_ATOM_ATOM: u32 = 4;
pub const XCB_ATOM_STRING: u32 = 31;
pub const XCB_ATOM_WM_NAME: u32 = 39;

pub const XCB_IMAGE_FORMAT_Z_PIXMAP: u8 = 2;

pub const IPC_PRIVATE: c_int = 0;
pub const IPC_CREAT: c_int = 0o1000;
pub const IPC_RMID: c_int = 0;

#[link(name = "xcb")]
unsafe extern "C" {
    pub fn xcb_connect(displayname: *const c_char, screenp: *mut c_int) -> *mut xcb_connection_t;
    pub fn xcb_disconnect(c: *mut xcb_connection_t);
    pub fn xcb_connection_has_error(c: *mut xcb_connection_t) -> c_int;
    pub fn xcb_get_setup(c: *mut xcb_connection_t) -> *const xcb_setup_t;
    pub fn xcb_setup_roots_iterator(r: *const xcb_setup_t) -> xcb_screen_iterator_t;
    pub fn xcb_screen_next(i: *mut xcb_screen_iterator_t);
    pub fn xcb_generate_id(c: *mut xcb_connection_t) -> u32;
    pub fn xcb_get_file_descriptor(c: *mut xcb_connection_t) -> c_int;
    pub fn xcb_get_maximum_request_length(c: *mut xcb_connection_t) -> u32;
    pub fn xcb_flush(c: *mut xcb_connection_t) -> c_int;
    pub fn xcb_poll_for_event(c: *mut xcb_connection_t) -> *mut xcb_generic_event_t;
    pub fn xcb_request_check(
        c: *mut xcb_connection_t,
        cookie: xcb_void_cookie_t,
    ) -> *mut xcb_generic_error_t;
    pub fn xcb_create_window(
        c: *mut xcb_connection_t,
        depth: u8,
        wid: u32,
        parent: u32,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        border_width: u16,
        class: u16,
        visual: u32,
        value_mask: u32,
        value_list: *const c_void,
    ) -> xcb_void_cookie_t;
    pub fn xcb_map_window(c: *mut xcb_connection_t, window: u32) -> xcb_void_cookie_t;
    pub fn xcb_change_property(
        c: *mut xcb_connection_t,
        mode: u8,
        window: u32,
        property: u32,
        type_: u32,
        format: u8,
        data_len: u32,
        data: *const c_void,
    ) -> xcb_void_cookie_t;
    pub fn xcb_intern_atom(
        c: *mut xcb_connection_t,
        only_if_exists: u8,
        name_len: u16,
        name: *const c_char,
    ) -> xcb_cookie_t;
    pub fn xcb_intern_atom_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_intern_atom_reply_t;
    pub fn xcb_create_gc(
        c: *mut xcb_connection_t,
        cid: u32,
        drawable: u32,
        value_mask: u32,
        value_list: *const c_void,
    ) -> xcb_void_cookie_t;
    pub fn xcb_put_image(
        c: *mut xcb_connection_t,
        format: u8,
        drawable: u32,
        gc: u32,
        width: u16,
        height: u16,
        dst_x: i16,
        dst_y: i16,
        left_pad: u8,
        depth: u8,
        data_len: u32,
        data: *const u8,
    ) -> xcb_void_cookie_t;
    pub fn xcb_get_keyboard_mapping(
        c: *mut xcb_connection_t,
        first_keycode: u8,
        count: u8,
    ) -> xcb_cookie_t;
    pub fn xcb_get_keyboard_mapping_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_get_keyboard_mapping_reply_t;
    pub fn xcb_get_keyboard_mapping_keysyms(r: *const xcb_get_keyboard_mapping_reply_t)
    -> *mut u32;
    pub fn xcb_get_keyboard_mapping_keysyms_length(
        r: *const xcb_get_keyboard_mapping_reply_t,
    ) -> c_int;
    pub fn xcb_get_input_focus(c: *mut xcb_connection_t) -> xcb_cookie_t;
    pub fn xcb_get_input_focus_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_get_input_focus_reply_t;
}

#[link(name = "xcb-shm")]
unsafe extern "C" {
    pub fn xcb_shm_query_version(c: *mut xcb_connection_t) -> xcb_cookie_t;
    pub fn xcb_shm_query_version_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_shm_query_version_reply_t;
    pub fn xcb_shm_attach_checked(
        c: *mut xcb_connection_t,
        shmseg: u32,
        shmid: u32,
        read_only: u8,
    ) -> xcb_void_cookie_t;
    pub fn xcb_shm_detach(c: *mut xcb_connection_t, shmseg: u32) -> xcb_void_cookie_t;
    pub fn xcb_shm_put_image(
        c: *mut xcb_connection_t,
        drawable: u32,
        gc: u32,
        total_width: u16,
        total_height: u16,
        src_x: u16,
        src_y: u16,
        src_width: u16,
        src_height: u16,
        dst_x: i16,
        dst_y: i16,
        depth: u8,
        format: u8,
        send_event: u8,
        shmseg: u32,
        offset: u32,
    ) -> xcb_void_cookie_t;
}

// glibc (libc.so.6, linked by std).
unsafe extern "C" {
    pub fn free(p: *mut c_void);
    pub fn shmget(key: c_int, size: usize, shmflg: c_int) -> c_int;
    pub fn shmat(shmid: c_int, shmaddr: *const c_void, shmflg: c_int) -> *mut c_void;
    pub fn shmdt(shmaddr: *const c_void) -> c_int;
    pub fn shmctl(shmid: c_int, cmd: c_int, buf: *mut c_void) -> c_int;
    pub fn poll(fds: *mut pollfd, nfds: u64, timeout: c_int) -> c_int;
}

#[repr(C)]
pub struct pollfd {
    pub fd: c_int,
    pub events: i16,
    pub revents: i16,
}

pub const POLLIN: i16 = 1;
