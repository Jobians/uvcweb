#![allow(dead_code)]
//! Hand-written bindings for the handful of libusb / libuvc functions we use.
//! Struct layouts mirror libusb-1.0 (`libusb.h`) and are stable across 1.0.x.
//! libuvc's own structs are never touched except the first four fields of
//! `uvc_frame_t`, and the stream-control struct is treated as an opaque blob.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};

// Opaque handles.
pub enum UsbHandle {}
pub enum UsbDevice {}
pub enum UvcContext {}
pub enum UvcDevHandle {}

pub const TRANSFER_COMPLETED: c_int = 0;
pub const TRANSFER_CANCELLED: c_int = 3;
pub const TRANSFER_TYPE_ISO: u8 = 1;
pub const LIBUSB_ERROR_BUSY: c_int = -6;
pub const LIBUSB_OPTION_LOG_LEVEL: c_int = 0;
pub const LIBUSB_LOG_LEVEL_ERROR: c_int = 1;
pub const LIBUSB_OPTION_NO_DEVICE_DISCOVERY: c_int = 2;

/// `UVC_FRAME_FORMAT_MJPEG` in libuvc's `enum uvc_frame_format`
/// (UNKNOWN=0, UNCOMPRESSED=1, COMPRESSED=2, YUYV=3, UYVY=4, RGB=5, BGR=6, MJPEG=7).
pub const UVC_FRAME_FORMAT_MJPEG: c_int = 7;

// ---- libusb config descriptor tree (read only) ----

#[repr(C)]
pub struct EndpointDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub b_endpoint_address: u8,
    pub bm_attributes: u8,
    pub w_max_packet_size: u16,
    pub b_interval: u8,
    pub b_refresh: u8,
    pub b_synch_address: u8,
    pub extra: *const u8,
    pub extra_length: c_int,
}

#[repr(C)]
pub struct InterfaceDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub b_interface_number: u8,
    pub b_alternate_setting: u8,
    pub b_num_endpoints: u8,
    pub b_interface_class: u8,
    pub b_interface_sub_class: u8,
    pub b_interface_protocol: u8,
    pub i_interface: u8,
    pub endpoint: *const EndpointDescriptor,
    pub extra: *const u8,
    pub extra_length: c_int,
}

#[repr(C)]
pub struct Interface {
    pub altsetting: *const InterfaceDescriptor,
    pub num_altsetting: c_int,
}

#[repr(C)]
pub struct ConfigDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub w_total_length: u16,
    pub b_num_interfaces: u8,
    pub b_configuration_value: u8,
    pub i_configuration: u8,
    pub bm_attributes: u8,
    pub max_power: u8,
    pub interface: *const Interface,
    pub extra: *const u8,
    pub extra_length: c_int,
}

// ---- libusb transfers ----

#[repr(C)]
pub struct IsoPacketDesc {
    pub length: c_uint,
    pub actual_length: c_uint,
    pub status: c_int,
}

pub type TransferCb = extern "C" fn(*mut Transfer);

#[repr(C)]
pub struct Transfer {
    pub dev_handle: *mut UsbHandle,
    pub flags: u8,
    pub endpoint: u8,
    pub kind: u8, // `type` in C
    pub timeout: c_uint,
    pub status: c_int,
    pub length: c_int,
    pub actual_length: c_int,
    pub callback: Option<TransferCb>,
    pub user_data: *mut c_void,
    pub buffer: *mut u8,
    pub num_iso_packets: c_int,
    pub iso_packet_desc: [IsoPacketDesc; 0], // flexible array member
}

// The "android-static" feature (only on for the Android app build; see build.rs and
// android/build-rust.sh) links libusb and libuvc statically from one archive that
// android/build-native-deps.sh produces. Everywhere else - including Termux, even though it
// also reports target_os = "android" - the system's shared libraries are used, the ones
// `pkg install libusb libuvc` / `apt install libusb-1.0-0-dev libuvc-dev` provide.
#[cfg_attr(not(feature = "android-static"), link(name = "usb-1.0"))]
#[cfg_attr(feature = "android-static", link(name = "uvcusb", kind = "static"))]
extern "C" {
    pub fn libusb_set_option(ctx: *mut c_void, option: c_int, ...) -> c_int;
    pub fn libusb_error_name(code: c_int) -> *const c_char;
    pub fn libusb_get_device(h: *mut UsbHandle) -> *mut UsbDevice;
    pub fn libusb_get_active_config_descriptor(
        dev: *mut UsbDevice,
        out: *mut *mut ConfigDescriptor,
    ) -> c_int;
    pub fn libusb_free_config_descriptor(cfg: *mut ConfigDescriptor);
    pub fn libusb_claim_interface(h: *mut UsbHandle, iface: c_int) -> c_int;
    pub fn libusb_release_interface(h: *mut UsbHandle, iface: c_int) -> c_int;
    pub fn libusb_set_interface_alt_setting(h: *mut UsbHandle, iface: c_int, alt: c_int) -> c_int;
    pub fn libusb_set_auto_detach_kernel_driver(h: *mut UsbHandle, enable: c_int) -> c_int;
    pub fn libusb_control_transfer(
        h: *mut UsbHandle,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: *mut u8,
        length: u16,
        timeout_ms: c_uint,
    ) -> c_int;
    pub fn libusb_alloc_transfer(iso_packets: c_int) -> *mut Transfer;
    pub fn libusb_free_transfer(t: *mut Transfer);
    pub fn libusb_submit_transfer(t: *mut Transfer) -> c_int;
    pub fn libusb_cancel_transfer(t: *mut Transfer) -> c_int;
}

// ---- libuvc ----

/// First fields of `uvc_frame_t`; the real struct continues, we never read past these.
#[repr(C)]
pub struct UvcFrame {
    pub data: *mut c_void,
    pub data_bytes: usize,
    pub width: u32,
    pub height: u32,
}

pub type FrameCb = extern "C" fn(*mut UvcFrame, *mut c_void);

#[cfg_attr(not(feature = "android-static"), link(name = "uvc"))]
extern "C" {
    pub fn uvc_init(ctx: *mut *mut UvcContext, usb_ctx: *mut c_void) -> c_int;
    pub fn uvc_exit(ctx: *mut UvcContext);
    pub fn uvc_wrap(sys_dev: c_int, ctx: *mut UvcContext, devh: *mut *mut UvcDevHandle) -> c_int;
    pub fn uvc_close(devh: *mut UvcDevHandle);
    pub fn uvc_get_libusb_handle(devh: *mut UvcDevHandle) -> *mut UsbHandle;
    pub fn uvc_get_stream_ctrl_format_size(
        devh: *mut UvcDevHandle,
        ctrl: *mut c_void,
        format: c_int,
        width: c_int,
        height: c_int,
        fps: c_int,
    ) -> c_int;
    pub fn uvc_start_streaming(
        devh: *mut UvcDevHandle,
        ctrl: *mut c_void,
        cb: FrameCb,
        user: *mut c_void,
        flags: u8,
    ) -> c_int;
    pub fn uvc_stop_streaming(devh: *mut UvcDevHandle);
    pub fn uvc_strerror(err: c_int) -> *const c_char;
    pub fn uvc_print_diag(devh: *mut UvcDevHandle, stream: *mut c_void);
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return "?".to_string();
    }
    unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
}

pub fn usb_err(code: c_int) -> String {
    cstr(unsafe { libusb_error_name(code) })
}

pub fn uvc_err(code: c_int) -> String {
    cstr(unsafe { uvc_strerror(code) })
}
