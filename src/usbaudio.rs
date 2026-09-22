//! Audio straight from the capture card's own USB Audio Class interface, via
//! libusb isochronous transfers. No ALSA and no root needed.
//!
//! libuvc already runs a libusb event thread on the device's context, so our
//! transfer callbacks are delivered by that thread.

use crate::descriptors::{self, AudioAlt};
use crate::ffi::*;
use crate::hub::{self, AudioFormat};
use std::os::raw::{c_int, c_uint};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

const XFERS: usize = 8; // transfers kept in flight
const PKTS: usize = 10; // iso packets (about 1 ms each) per transfer

struct Ctl {
    handle: *mut UsbHandle,
    interface: c_int,
    claimed: bool,
    alt_set: bool,
    xfers: Vec<*mut Transfer>,
    bufs: Vec<Vec<u8>>, // owned here, referenced by the transfers
}

// The raw pointers are only ever used under the rules of libusb's thread safety.
unsafe impl Send for Ctl {}

static CTL: Mutex<Option<Ctl>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static ACTIVE: Mutex<i32> = Mutex::new(0); // transfers currently submitted
static ACTIVE_CV: Condvar = Condvar::new();
static FRAME_BYTES: AtomicUsize = AtomicUsize::new(4);
static PKT_ERRORS: AtomicU64 = AtomicU64::new(0);
static REARM_LOGS: AtomicUsize = AtomicUsize::new(0);

fn active() -> MutexGuard<'static, i32> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Find, claim and start capturing the card's audio. On success the hub knows the audio format.
pub fn start(handle: *mut UsbHandle, want_rate: u32, want_ch: u16) -> Result<(), String> {
    let alts = unsafe { descriptors::read_active_config(handle)? };
    let cands = descriptors::audio_alts(&alts);
    for a in &cands {
        say!(
            "USB audio: found if {} alt {} ep 0x{:02x} maxpkt {}: {} ch, {}-bit, {}",
            a.interface,
            a.alt,
            a.endpoint,
            a.max_packet,
            a.channels,
            a.bits,
            describe_rates(a)
        );
    }
    let a: AudioAlt = match descriptors::choose_audio(&cands, want_rate, want_ch) {
        Some(x) => x.clone(),
        None => return Err("no usable 16-bit PCM capture interface on this device".to_string()),
    };
    let rate = descriptors::pick_rate(&a, want_rate);
    say!("USB audio: using if {} alt {} ep 0x{:02x}: {} Hz, {} ch, S16_LE", a.interface, a.alt, a.endpoint, rate, a.channels);
    if rate != want_rate {
        say!("USB audio: card doesn't offer {} Hz, using {} Hz", want_rate, rate);
    }
    if a.channels as u16 != want_ch {
        say!("USB audio: card is {} ch, using that instead of {}", a.channels, want_ch);
    }

    unsafe {
        libusb_set_auto_detach_kernel_driver(handle, 1); // let libusb take it from snd-usb-audio if needed
        let iface = a.interface as c_int;
        let r = libusb_claim_interface(handle, iface);
        if r < 0 {
            let hint = if r == LIBUSB_ERROR_BUSY {
                " (another driver, probably Android's own USB audio, owns it)"
            } else {
                ""
            };
            return Err(format!("cannot claim interface {}: {}{}", iface, usb_err(r), hint));
        }
        let mut ctl = Ctl { handle, interface: iface, claimed: true, alt_set: false, xfers: Vec::new(), bufs: Vec::new() };

        let r = libusb_set_interface_alt_setting(handle, iface, a.alt as c_int);
        if r < 0 {
            let msg = format!("cannot select alt setting {}: {}", a.alt, usb_err(r));
            release(&mut ctl);
            return Err(msg);
        }
        ctl.alt_set = true;

        if a.freq_ctl {
            // UAC1 SET_CUR SAMPLING_FREQ_CONTROL on the endpoint
            let mut f = [(rate & 0xFF) as u8, ((rate >> 8) & 0xFF) as u8, ((rate >> 16) & 0xFF) as u8];
            let r = libusb_control_transfer(handle, 0x22, 0x01, 0x0100, a.endpoint as u16, f.as_mut_ptr(), 3, 1000);
            if r < 0 {
                say!("USB audio: setting sample rate failed ({}), continuing", usb_err(r));
            }
        }

        FRAME_BYTES.store(a.channels as usize * 2, Ordering::SeqCst);
        PKT_ERRORS.store(0, Ordering::SeqCst);
        REARM_LOGS.store(0, Ordering::SeqCst);

        // allocate the iso transfers
        let bufsz = PKTS * a.max_packet;
        for _ in 0..XFERS {
            let t = libusb_alloc_transfer(PKTS as c_int);
            if t.is_null() {
                break;
            }
            let mut buf = vec![0u8; bufsz];
            (*t).dev_handle = handle;
            (*t).flags = 0;
            (*t).endpoint = a.endpoint;
            (*t).kind = TRANSFER_TYPE_ISO;
            (*t).timeout = 0;
            (*t).length = bufsz as c_int;
            (*t).callback = Some(iso_cb);
            (*t).user_data = std::ptr::null_mut();
            (*t).buffer = buf.as_mut_ptr();
            (*t).num_iso_packets = PKTS as c_int;
            let descs = std::ptr::addr_of_mut!((*t).iso_packet_desc) as *mut IsoPacketDesc;
            for i in 0..PKTS {
                (*descs.add(i)).length = a.max_packet as c_uint;
            }
            ctl.xfers.push(t);
            ctl.bufs.push(buf); // moving the Vec does not move its heap buffer
        }

        RUNNING.store(true, Ordering::SeqCst);
        let mut ok = 0usize;
        let mut first_err = 0;
        for &t in &ctl.xfers {
            *active() += 1;
            let r = libusb_submit_transfer(t);
            if r == 0 {
                ok += 1;
            } else {
                *active() -= 1;
                if first_err == 0 {
                    first_err = r;
                }
            }
        }
        if ok == 0 {
            let msg = format!("submit failed: {}", usb_err(first_err));
            *CTL.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctl);
            stop();
            return Err(msg);
        }
        if let Some(h) = hub::try_global() {
            h.set_audio_format(AudioFormat { rate, channels: a.channels as u16 });
        }
        say!("USB audio capture started ({} transfers x {} packets, {} bytes/packet)", ok, PKTS, a.max_packet);
        *CTL.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctl);
    }
    Ok(())
}

fn describe_rates(a: &AudioAlt) -> String {
    if a.rates.is_empty() {
        format!("{}-{} Hz", a.rate_min, a.rate_max)
    } else {
        let v: Vec<String> = a.rates.iter().map(|r| r.to_string()).collect();
        format!("{} Hz", v.join("/"))
    }
}

/// Undo claim / alt setting (used on failure paths and by `stop`).
unsafe fn release(ctl: &mut Ctl) {
    if ctl.alt_set {
        libusb_set_interface_alt_setting(ctl.handle, ctl.interface, 0);
        ctl.alt_set = false;
    }
    if ctl.claimed {
        libusb_release_interface(ctl.handle, ctl.interface);
        ctl.claimed = false;
    }
}

/// Cancel all transfers, wait until libusb has returned them, then release the interface.
pub fn stop() {
    let taken = CTL.lock().unwrap_or_else(|e| e.into_inner()).take();
    let mut ctl = match taken {
        Some(c) => c,
        None => return,
    };
    RUNNING.store(false, Ordering::SeqCst);
    // A callback racing with the first cancel may resubmit once more, hence the loop.
    for _ in 0..20 {
        for &t in &ctl.xfers {
            unsafe {
                libusb_cancel_transfer(t);
            }
        }
        let mut g = active();
        if *g > 0 {
            let (ng, _) = ACTIVE_CV.wait_timeout(g, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner());
            g = ng;
        }
        let left = *g;
        drop(g);
        if left == 0 {
            break;
        }
    }
    let left = *active();
    unsafe {
        if left == 0 {
            for &t in &ctl.xfers {
                libusb_free_transfer(t);
            }
            ctl.xfers.clear();
            ctl.bufs.clear();
        } else {
            say!("USB audio: {} transfers did not cancel, leaving them", left);
            let leaked = std::mem::take(&mut ctl.bufs);
            std::mem::forget(leaked); // the kernel may still write into them
        }
        release(&mut ctl);
    }
    say!("USB audio stopped");
}

/// Called once a second: if every transfer has ended, try to bring them back.
pub fn poll() {
    if !RUNNING.load(Ordering::SeqCst) {
        return;
    }
    if *active() > 0 {
        return;
    }
    let guard = CTL.lock().unwrap_or_else(|e| e.into_inner());
    let ctl = match guard.as_ref() {
        Some(c) => c,
        None => return,
    };
    if REARM_LOGS.fetch_add(1, Ordering::SeqCst) < 5 {
        say!("USB audio: all transfers ended ({} bad packets) - re-arming", PKT_ERRORS.load(Ordering::SeqCst));
    }
    for &t in &ctl.xfers {
        *active() += 1;
        if unsafe { libusb_submit_transfer(t) } != 0 {
            *active() -= 1;
        }
    }
}

extern "C" fn iso_cb(t: *mut Transfer) {
    // A panic must never unwind into C.
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe { iso_cb_inner(t) }));
}

unsafe fn iso_cb_inner(t: *mut Transfer) {
    let status = (*t).status;
    if status == TRANSFER_COMPLETED {
        let n = (*t).num_iso_packets as usize;
        let descs = std::ptr::addr_of!((*t).iso_packet_desc) as *const IsoPacketDesc;
        let buf = (*t).buffer as *const u8;
        let fb = FRAME_BYTES.load(Ordering::Relaxed).max(1);
        let session = hub::try_global(); // None once the session is over: data is dropped
        let mut offset = 0usize; // packets sit at fixed offsets: the sum of the requested lengths
        for i in 0..n {
            let d = &*descs.add(i);
            if d.status != 0 {
                PKT_ERRORS.fetch_add(1, Ordering::Relaxed);
            } else {
                let got = d.actual_length as usize;
                let usable = got - (got % fb);
                if usable > 0 {
                    if let Some(h) = &session {
                        h.push_audio(std::slice::from_raw_parts(buf.add(offset), usable));
                    }
                }
            }
            offset += d.length as usize;
        }
        if RUNNING.load(Ordering::SeqCst) && libusb_submit_transfer(t) == 0 {
            return; // resubmitted, still in flight
        }
    } else if status != TRANSFER_CANCELLED {
        say!("USB audio: transfer ended (status {})", status);
    }
    let mut g = active();
    *g -= 1;
    ACTIVE_CV.notify_all();
}
