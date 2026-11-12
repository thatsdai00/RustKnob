// Logitech PRO X SUPERLIGHT 2 over HID++ 2.0 (no G HUB): battery, DPI and wake-ups, reads only. Driven by the
// mouse thread in mouse.rs, which treats it like the Razer mouse.
//
// HID++ long reports (id 0x11, 20 bytes) go to and come from the receiver's (or the cable's) vendor collection
// (usage page 0xFF00, usage 2):
//
//   request  [0x11, device, feature index, function << 4 | software id, params...]
//   answer   the same four bytes, then the result
//   error    [0x11, device, 0xFF, feature index, function << 4 | software id, error code]
//
// Feature indexes differ per device: the root feature (index 0) maps a feature id to its index. Used here:
//   0x1004 unified battery  fn 1: [percent, level, charging (0 no, 1 yes, 2 slowly, 3 full, 4 error), ...]
//   0x1000 battery status   fn 0: [percent, next level, status (1..4 on the charger)]
//   0x1001 battery voltage  fn 0: [mV hi, mV lo, flags (bit 7: on the charger)]
//   0x2201 adjustable DPI   fn 2: [sensor, dpi hi, dpi lo, ...]
// A mouse that is asleep or out of range does not answer through the receiver: that counts as asleep.
//
// Not tried on real hardware here (no Superlight 2 at hand): the ids and the protocol follow Logitech's
// HID++ 2.0 and what Linux (hid-logitech-dj / -hidpp), Solaar and libratbag do with this mouse.

use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Devices::HumanInterfaceDevice::{HidD_GetAttributes, HidD_SetOutputReport, HIDD_ATTRIBUTES};
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::mouse::Error;

const VID: u16 = 0x046d;
/// The Superlight 2's Lightspeed receiver.
const RECEIVERS: [u16; 1] = [0xc54d];
/// The Superlight 2 on its cable.
const WIRED: [u16; 1] = [0xc09b];
const LONG: usize = 20;
const SOFTWARE_ID: u8 = 0x0a;
const ANSWER_WITHIN: Duration = Duration::from_millis(250);

pub struct Logi {
    handle: HANDLE,
    event: HANDLE,
    pub path: Vec<u16>,
    device: u8,           // 1: the first device on a receiver; 0xFF: the mouse itself on its cable
    features: Option<Features>, // looked up once the mouse answers
}

#[derive(Clone, Copy)]
struct Features {
    battery: Option<(u8, u16)>, // index, and which battery feature it is
    dpi: Option<u8>,
}

impl Drop for Logi {
    fn drop(&mut self) {
        unsafe {
            let _ = CancelIoEx(self.handle, None);
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.event);
        }
    }
}

impl Logi {
    /// The path to talk to: the cable's when it is plugged in, else the receiver's.
    /// The long HID++ collection (vendor page 0xFF00, usage 2, 20-byte reports) of the cable or the receiver.
    pub fn preferred() -> Option<Vec<u16>> {
        let all = unsafe { crate::mouse::hid_list("vid_046d") };
        let hidpp = |d: &&crate::mouse::HidInfo| d.vid == VID && d.usage_page == 0xff00 && d.usage == 0x0002 && d.input_len as usize == LONG;
        let pick = |pids: &[u16]| all.iter().filter(hidpp).find(|d| pids.contains(&d.pid)).map(|d| d.path.clone());
        pick(&WIRED).or_else(|| pick(&RECEIVERS))
    }

    pub fn open(path: Vec<u16>) -> Option<Logi> {
        unsafe {
            let handle = CreateFileW(PCWSTR(path.as_ptr()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAG_OVERLAPPED, None).ok().filter(|h| !h.is_invalid())?;
            let Ok(event) = CreateEventW(None, true, false, PCWSTR::null()) else {
                let _ = CloseHandle(handle);
                return None;
            };
            let mut attr = HIDD_ATTRIBUTES { Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
            let _ = HidD_GetAttributes(handle, &mut attr);
            let device = if WIRED.contains(&attr.ProductID) { 0xff } else { 1 };
            Some(Logi { handle, event, path, device, features: None })
        }
    }

    /// One request and its answer (the 16 bytes after the header).
    fn request(&mut self, feature: u8, function: u8, params: &[u8]) -> Result<[u8; 16], Error> {
        let mut out = [0u8; LONG];
        out[0] = 0x11;
        out[1] = self.device;
        out[2] = feature;
        out[3] = function << 4 | SOFTWARE_ID;
        out[4..4 + params.len().min(16)].copy_from_slice(&params[..params.len().min(16)]);
        unsafe {
            if !HidD_SetOutputReport(self.handle, out.as_ptr() as *const _, LONG as u32).as_bool() {
                // some receivers only take it as a plain write
                let mut ov = OVERLAPPED { hEvent: self.event, ..Default::default() };
                let _ = ResetEvent(self.event);
                let mut n = 0u32;
                if WriteFile(self.handle, Some(&out), None, Some(&mut ov)).is_err() && GetLastError() != ERROR_IO_PENDING {
                    return Err(Error::Io);
                }
                if WaitForSingleObject(self.event, 200) != WAIT_OBJECT_0 {
                    let _ = CancelIoEx(self.handle, Some(&ov));
                    let _ = GetOverlappedResult(self.handle, &ov, &mut n, true);
                    return Err(Error::Asleep);
                }
                if GetOverlappedResult(self.handle, &ov, &mut n, false).is_err() {
                    return Err(Error::Io);
                }
            }
            // answers (and notifications, which are skipped) arrive as input reports
            let until = Instant::now() + ANSWER_WITHIN;
            loop {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(Error::Asleep);
                }
                let r = self.read(left.as_millis() as u32)?;
                let Some(r) = r else { return Err(Error::Asleep) };
                if r[0] != 0x11 || r[1] != self.device {
                    continue;
                }
                if r[2] == 0xff && r[3] == feature && r[4] == out[3] {
                    return Err(Error::Asleep); // an error answer: not there, or not supported
                }
                if r[2] == feature && r[3] == out[3] {
                    let mut a = [0u8; 16];
                    a.copy_from_slice(&r[4..20]);
                    return Ok(a);
                }
            }
        }
    }

    /// One input report within `ms`, or None.
    unsafe fn read(&mut self, ms: u32) -> Result<Option<[u8; LONG]>, Error> {
        let mut buf = [0u8; LONG];
        let mut ov = OVERLAPPED { hEvent: self.event, ..Default::default() };
        let _ = ResetEvent(self.event);
        let mut n = 0u32;
        if ReadFile(self.handle, Some(&mut buf), None, Some(&mut ov)).is_err() && GetLastError() != ERROR_IO_PENDING {
            return Err(Error::Io);
        }
        if WaitForSingleObject(self.event, ms) != WAIT_OBJECT_0 {
            let _ = CancelIoEx(self.handle, Some(&ov));
            let _ = GetOverlappedResult(self.handle, &ov, &mut n, true);
            return Ok(None);
        }
        if GetOverlappedResult(self.handle, &ov, &mut n, false).is_err() {
            return Err(Error::Io);
        }
        Ok((n as usize >= LONG).then_some(buf))
    }

    /// The index of a feature (root feature, function 0); None when the mouse does not have it.
    fn feature(&mut self, id: u16) -> Result<Option<u8>, Error> {
        let a = self.request(0x00, 0, &id.to_be_bytes())?;
        Ok((a[0] != 0).then_some(a[0]))
    }

    /// Looks the features up the first time the mouse answers.
    fn features(&mut self) -> Result<Features, Error> {
        if let Some(f) = self.features {
            return Ok(f);
        }
        let mut battery = None;
        for id in [0x1004, 0x1000, 0x1001] {
            if let Some(index) = self.feature(id)? {
                battery = Some((index, id));
                break;
            }
        }
        let f = Features { battery, dpi: self.feature(0x2201)? };
        self.features = Some(f);
        Ok(f)
    }

    /// The DPI (0 when the mouse does not tell it); answering at all means it is awake.
    pub fn dpi(&mut self) -> Result<u16, Error> {
        let f = self.features()?;
        match f.dpi {
            Some(index) => {
                let a = self.request(index, 2, &[0])?;
                Ok(u16::from_be_bytes([a[1], a[2]]))
            }
            None => {
                self.request(0x00, 1, &[0, 0, 0x5a])?; // a ping
                Ok(0)
            }
        }
    }

    /// (percent, on the charger).
    pub fn battery(&mut self) -> Result<(Option<u8>, bool), Error> {
        let Some((index, id)) = self.features()?.battery else { return Ok((None, false)) };
        Ok(match id {
            0x1004 => {
                let a = self.request(index, 1, &[])?;
                (Some(a[0].min(100)), matches!(a[2], 1..=3))
            }
            0x1000 => {
                let a = self.request(index, 0, &[])?;
                (Some(a[0].min(100)), matches!(a[2], 1..=4))
            }
            _ => {
                let a = self.request(index, 0, &[])?;
                (Some(percent_from_mv(u16::from_be_bytes([a[0], a[1]]))), a[2] & 0x80 != 0)
            }
        })
    }
}

/// A lithium cell's charge from its voltage (the curve libratbag and Solaar use, coarsely).
fn percent_from_mv(mv: u16) -> u8 {
    const CURVE: [(u16, u8); 11] = [(4186, 100), (4067, 90), (3989, 80), (3922, 70), (3859, 60), (3811, 50), (3778, 40), (3751, 30), (3717, 20), (3671, 10), (3500, 0)];
    if mv >= CURVE[0].0 {
        return 100;
    }
    for w in CURVE.windows(2) {
        let ((hi_mv, hi), (lo_mv, lo)) = (w[0], w[1]);
        if mv >= lo_mv {
            let t = (mv - lo_mv) as f32 / (hi_mv - lo_mv) as f32;
            return (lo as f32 + t * (hi - lo) as f32).round() as u8;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn voltage_curve() {
        assert_eq!(super::percent_from_mv(4200), 100);
        assert_eq!(super::percent_from_mv(3811), 50);
        assert_eq!(super::percent_from_mv(3400), 0);
        assert!((super::percent_from_mv(3790) as i32 - 44).abs() <= 2);
    }
}
