//! Camera side: open the card through libuvc, pick an MJPEG mode, start streaming.
//! Every frame is handed to the hub; nothing here knows about protocols.

use crate::config::Config;
use crate::descriptors;
use crate::ffi::*;
use crate::hub;
use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

pub struct Capture {
    ctx: *mut UvcContext,
    devh: *mut UvcDevHandle,
    ctrl: [u64; 32],
    // libuvc's uvc_stream_ctrl_t is treated as an opaque blob (256 bytes is plenty)
    mode: (i32, i32, i32),
}

// Only ever used from the main thread; the raw pointers just stop the auto trait.
unsafe impl Send for Capture {}

extern "C" fn on_frame(f: *mut UvcFrame, _arg: *mut c_void) {
    // A panic must never unwind into C.
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        if f.is_null() {
            return;
        }
        let p = (*f).data as *const u8;
        let n = (*f).data_bytes;
        if p.is_null() || n == 0 {
            return;
        }
        if let Some(h) = hub::try_global() {
            h.submit_frame(std::slice::from_raw_parts(p, n), (*f).width, (*f).height);
        }
    }));
}

unsafe fn device_ids(h: *mut UsbHandle) -> Option<(u16, u16)> {
    // standard GET_DESCRIPTOR(device): idVendor at bytes 8-9, idProduct at 10-11
    let mut buf = [0u8; 18];
    let n = libusb_control_transfer(h, 0x80, 0x06, 0x0100, 0, buf.as_mut_ptr(), 18, 1000);
    if n < 12 {
        return None;
    }
    Some((
        u16::from_le_bytes([buf[8], buf[9]]),
        u16::from_le_bytes([buf[10], buf[11]]),
    ))
}

impl Capture {
    /// Open the card. On failure returns the process exit code (same numbers as the C version).
    pub fn open(cfg: &Config) -> Result<Capture, i32> {
        unsafe {
            // Enable libusb debug logging.
            let r = libusb_set_option(
                std::ptr::null_mut(),
                LIBUSB_OPTION_LOG_LEVEL,
                LIBUSB_LOG_LEVEL_ERROR,
            );
            if r != 0 {
                say!("warning: libusb log-level option failed: {}", usb_err(r));
            }

            // Don't let libusb rediscover devices; use the FD supplied by termux-usb.
            let r = libusb_set_option(std::ptr::null_mut(), LIBUSB_OPTION_NO_DEVICE_DISCOVERY);
            if r != 0 {
                say!(
                    "warning: libusb device-discovery option failed: {}",
                    usb_err(r)
                );
            }
            let mut ctx: *mut UvcContext = std::ptr::null_mut();
            let e = uvc_init(&mut ctx, std::ptr::null_mut());
            if e < 0 {
                say!("uvc_init failed: {}", uvc_err(e));
                return Err(1);
            }
            let mut devh: *mut UvcDevHandle = std::ptr::null_mut();
            let e = uvc_wrap(cfg.fd, ctx, &mut devh);
            if e < 0 {
                say!(
          "uvc_wrap failed: {} (fd={}). Unplug/replug, run termux-usb -l, accept the popup.",
          uvc_err(e),
          cfg.fd
        );
                uvc_exit(ctx);
                return Err(2);
            }
            let usb = uvc_get_libusb_handle(devh);
            match device_ids(usb) {
                Some((v, p)) => say!("device opened: {:04x}:{:04x}", v, p),
                None => say!("device opened (no descriptor info)"),
            }

            let mw: i32;
            let mh: i32;
            let mf: i32;
            if cfg.width == 0 || cfg.height == 0 {
                let modes = match descriptors::read_active_config(usb) {
                    Ok(alts) => descriptors::mjpeg_modes(&alts),
                    Err(e) => {
                        say!("{}", e);
                        Vec::new()
                    }
                };
                match descriptors::default_mode(&modes) {
                    Some((w, h, f)) => {
                        let f = if cfg.fps > 0 { cfg.fps } else { f };
                        mw = w as i32;
                        mh = h as i32;
                        mf = f as i32;
                        say!(
                            "using the card's default mode: MJPEG {}x{} @ {} fps",
                            mw,
                            mh,
                            mf
                        );
                    }
                    None => {
                        uvc_print_diag(devh, std::ptr::null_mut());
                        say!("the card lists no MJPEG mode (this program only handles MJPEG)");
                        uvc_close(devh);
                        uvc_exit(ctx);
                        return Err(3);
                    }
                }
            } else {
                mw = cfg.width as i32;
                mh = cfg.height as i32;
                mf = if cfg.fps > 0 { cfg.fps as i32 } else { 30 };
                say!("using requested mode: MJPEG {}x{} @ {} fps", mw, mh, mf);
            }

            let mut cap = Capture {
                ctx,
                devh,
                ctrl: [0u64; 32],
                mode: (mw, mh, mf),
            };
            let e = uvc_get_stream_ctrl_format_size(
                devh,
                cap.ctrl.as_mut_ptr() as *mut c_void,
                UVC_FRAME_FORMAT_MJPEG,
                mw,
                mh,
                mf,
            );
            if e < 0 {
                uvc_print_diag(devh, std::ptr::null_mut());
                say!("MJPEG {}x{} @ {} not accepted: {}", mw, mh, mf, uvc_err(e));
                say!("pick a size from the list above and pass it with -w -h -f");
                uvc_close(devh);
                uvc_exit(ctx);
                return Err(3);
            }
            say!("video mode accepted, starting stream...");
            let e = uvc_start_streaming(
                devh,
                cap.ctrl.as_mut_ptr() as *mut c_void,
                on_frame,
                std::ptr::null_mut(),
                0,
            );
            if e < 0 {
                say!(
                    "start_streaming failed: {} (try a lower resolution or fps)",
                    uvc_err(e)
                );
                uvc_close(devh);
                uvc_exit(ctx);
                return Err(4);
            }
            say!("streaming started, waiting for first frame...");
            cap.ctx = ctx;
            Ok(cap)
        }
    }

    /// The libusb handle libuvc opened; the USB audio code shares it.
    pub fn usb_handle(&self) -> *mut UsbHandle {
        unsafe { uvc_get_libusb_handle(self.devh) }
    }

    /// Stop and start the video stream again (used when the card goes quiet).
    pub fn restart(&mut self) -> Result<(), String> {
        unsafe {
            uvc_stop_streaming(self.devh);
        }
        std::thread::sleep(Duration::from_millis(500));
        let (mw, mh, mf) = self.mode;
        unsafe {
            let mut e = uvc_get_stream_ctrl_format_size(
                self.devh,
                self.ctrl.as_mut_ptr() as *mut c_void,
                UVC_FRAME_FORMAT_MJPEG,
                mw,
                mh,
                mf,
            );
            if e >= 0 {
                e = uvc_start_streaming(
                    self.devh,
                    self.ctrl.as_mut_ptr() as *mut c_void,
                    on_frame,
                    std::ptr::null_mut(),
                    0,
                );
            }
            if e < 0 {
                return Err(uvc_err(e));
            }
        }
        Ok(())
    }

    pub fn close(self) {
        unsafe {
            uvc_stop_streaming(self.devh);
            uvc_close(self.devh);
            uvc_exit(self.ctx);
        }
    }
}
