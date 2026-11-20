// HID collections: finding them by vendor and usage, and talking to them with every wait bounded (a device that
// is not there must never hang the thread that asks). Shared by the mouse cards (mouse.rs and its brands) and
// the battery of every other device (battery/).

use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;

#[derive(Debug, PartialEq, Clone)]
pub enum Error {
    Io,     // transfer failed: unplugged
    Asleep, // the receiver answers for the mouse: asleep, off or out of range (or it did not answer at all)
}

/// Opens a HID collection with no access rights: enough for feature reports and for asking what it is (Windows
/// lets nobody open a mouse collection for reading: the input stack owns it).
pub unsafe fn open(path: &[u16]) -> Option<HANDLE> {
    CreateFileW(PCWSTR(path.as_ptr()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None).ok().filter(|h| !h.is_invalid())
}

/// One present HID collection.
#[derive(Clone)]
pub struct HidInfo {
    pub path: Vec<u16>,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub feature_len: u16,
    pub input_len: u16,
    pub output_len: u16,
    pub interface: Option<u8>,  // the "&mi_xx" of a composite device's path
    pub value_caps: (u16, u16), // how many (feature, input) values it declares
}

impl HidInfo {
    pub fn is(&self, vid: u16, pid: u16) -> bool {
        self.vid == vid && self.pid == pid
    }

    /// Its top-level usage is (`page`, `usage`).
    pub fn usage_is(&self, page: u16, usage: u16) -> bool {
        self.usage_page == page && self.usage == usage
    }
}

/// The product name the device gives, trimmed.
pub fn product(path: &[u16]) -> Option<String> {
    unsafe {
        let h = open(path)?;
        let mut buf = [0u16; 128];
        let ok = HidD_GetProductString(h, buf.as_mut_ptr() as *mut _, (buf.len() * 2) as u32).as_bool();
        let _ = CloseHandle(h);
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        ok.then(|| String::from_utf16_lossy(&buf[..len]).trim().to_string()).filter(|s| !s.is_empty())
    }
}

/// Every present HID collection whose path carries `vid_tag` ("vid_1532"): only those are opened.
pub unsafe fn hid_list(vid_tag: &str) -> Vec<HidInfo> {
    let mut out = Vec::new();
    let guid = HidD_GetHidGuid();
    let Ok(set) = SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE) else { return out };
    let mut i = 0u32;
    loop {
        let mut ifd = SP_DEVICE_INTERFACE_DATA { cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
        if SetupDiEnumDeviceInterfaces(set, None, &guid, i, &mut ifd).is_err() {
            break;
        }
        i += 1;
        let mut need = 0u32;
        let _ = SetupDiGetDeviceInterfaceDetailW(set, &ifd, None, 0, Some(&mut need), None);
        if need == 0 {
            continue;
        }
        let mut buf = vec![0u32; (need as usize).div_ceil(4)]; // aligned for the struct
        let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        if SetupDiGetDeviceInterfaceDetailW(set, &ifd, Some(detail), need, None, None).is_err() {
            continue;
        }
        let p = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
        let max = (need as usize).saturating_sub(4) / 2;
        let len = (0..max).find(|&k| *p.add(k) == 0).unwrap_or(max);
        let mut path = std::slice::from_raw_parts(p, len).to_vec();
        if !String::from_utf16_lossy(&path).to_ascii_lowercase().contains(vid_tag) {
            continue;
        }
        path.push(0);
        if let Some(info) = probe(path) {
            out.push(info);
        }
    }
    let _ = SetupDiDestroyDeviceInfoList(set);
    out
}

/// What a collection is: its ids, usage and report sizes.
pub unsafe fn probe(path: Vec<u16>) -> Option<HidInfo> {
    let h = open(&path)?;
    let mut attr = HIDD_ATTRIBUTES { Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
    let mut result = None;
    if HidD_GetAttributes(h, &mut attr).as_bool() {
        let mut pp = PHIDP_PREPARSED_DATA::default();
        if HidD_GetPreparsedData(h, &mut pp).as_bool() {
            let mut caps = HIDP_CAPS::default();
            if HidP_GetCaps(pp, &mut caps) == HIDP_STATUS_SUCCESS {
                let lower = String::from_utf16_lossy(&path).to_ascii_lowercase();
                let interface = lower.find("&mi_").and_then(|i| lower.get(i + 4..i + 6)).and_then(|h| u8::from_str_radix(h, 16).ok());
                result = Some(HidInfo {
                    path,
                    vid: attr.VendorID,
                    pid: attr.ProductID,
                    usage_page: caps.UsagePage,
                    usage: caps.Usage,
                    feature_len: caps.FeatureReportByteLength,
                    input_len: caps.InputReportByteLength,
                    output_len: caps.OutputReportByteLength,
                    interface,
                    value_caps: (caps.NumberFeatureValueCaps, caps.NumberInputValueCaps),
                });
            }
            let _ = HidD_FreePreparsedData(pp);
        }
    }
    let _ = CloseHandle(h);
    result
}

// ---- reports with timeouts (Logitech, ATK, ASUS, G435) -------------------------

/// A HID collection opened for output and input reports, every wait bounded (a mouse that is not there must
/// not hang the thread). Razer needs none of this: it talks through feature reports.
pub struct HidIo {
    handle: HANDLE,
    event: HANDLE,
    out_len: usize, // report sizes of the collection, report id included
    in_len: usize,
    feat_len: usize,
    pub gone: std::cell::Cell<bool>, // a transfer failed the way an unplugged device fails
}

/// A report as most vendor protocols count its bytes: an unnumbered report's leading 0 left out.
pub fn payload(report: &[u8]) -> &[u8] {
    match report {
        [0, rest @ ..] => rest,
        _ => report,
    }
}

// the handle and its event belong to whoever holds the HidIo: one thread at a time
unsafe impl Send for HidIo {}

impl Drop for HidIo {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::IO::CancelIoEx(self.handle, None);
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.event);
        }
    }
}

impl HidIo {
    pub fn open(path: &[u16]) -> Option<HidIo> {
        Self::open_as(&unsafe { probe(path.to_vec()) }?, (GENERIC_READ | GENERIC_WRITE).0, FILE_FLAG_OVERLAPPED)
    }

    /// Opened for reading only (input reports and feature reports): for collections that refuse to be written.
    pub fn open_read(info: &HidInfo) -> Option<HidIo> {
        Self::open_as(info, GENERIC_READ.0, FILE_FLAG_OVERLAPPED)
    }

    /// Opened with no rights at all: feature reports only (a mouse or keyboard collection the input stack owns).
    pub fn open_feature(info: &HidInfo) -> Option<HidIo> {
        Self::open_as(info, 0, FILE_FLAGS_AND_ATTRIBUTES(0))
    }

    fn open_as(info: &HidInfo, access: u32, flags: FILE_FLAGS_AND_ATTRIBUTES) -> Option<HidIo> {
        unsafe {
            let handle = CreateFileW(PCWSTR(info.path.as_ptr()), access, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, flags, None).ok().filter(|h| !h.is_invalid())?;
            let Ok(event) = windows::Win32::System::Threading::CreateEventW(None, true, false, PCWSTR::null()) else {
                let _ = CloseHandle(handle);
                return None;
            };
            Some(HidIo { handle, event, out_len: info.output_len as usize, in_len: info.input_len as usize, feat_len: info.feature_len as usize, gone: Default::default() })
        }
    }

    /// (output, feature) report sizes, report id included.
    pub fn lens(&self) -> (usize, usize) {
        (self.out_len, self.feat_len)
    }

    /// Sends `report` (its id first) over the control pipe (SET_REPORT), padded.
    pub fn set_output(&mut self, report: &[u8]) -> bool {
        let out = self.padded(report);
        unsafe { HidD_SetOutputReport(self.handle, out.as_ptr() as *const _, out.len() as u32) }.as_bool()
    }

    /// Forgets the input reports waiting in the driver's queue (so the next read is an answer, not an old report).
    pub fn flush(&mut self) {
        unsafe {
            let _ = HidD_FlushQueue(self.handle);
        }
    }

    /// Feature report `id`, the collection's whole length (the id first).
    pub fn get_feature(&mut self, id: u8) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; self.feat_len.max(2)];
        buf[0] = id;
        let ok = unsafe { HidD_GetFeature(self.handle, buf.as_mut_ptr() as *mut _, buf.len() as u32) }.as_bool();
        self.gone.set(!ok);
        ok.then_some(buf)
    }

    /// Sends `report` (its id first) as a feature report, padded to the collection's length.
    pub fn set_feature(&mut self, report: &[u8]) -> bool {
        let mut buf = report.to_vec();
        buf.resize(self.feat_len.max(report.len()), 0);
        let ok = unsafe { HidD_SetFeature(self.handle, buf.as_ptr() as *const _, buf.len() as u32) }.as_bool();
        self.gone.set(!ok);
        ok
    }

    /// Input report `id` asked for over the control pipe (GET_REPORT), the collection's whole length.
    pub fn get_input(&mut self, id: u8) -> Option<Vec<u8>> {
        let mut buf = self.input_buffer();
        buf[0] = id;
        unsafe { HidD_GetInputReport(self.handle, buf.as_mut_ptr() as *mut _, buf.len() as u32) }.as_bool().then_some(buf)
    }

    /// Sends `report` (its id first) as a plain write.
    pub fn write(&mut self, report: &[u8]) -> Result<(), Error> {
        unsafe { self.send(&self.padded(report), 500)? }.map(|_| ()).ok_or(Error::Asleep)
    }

    /// The next input report within `within` (None: none came).
    pub fn read(&mut self, within: Duration) -> Result<Option<Vec<u8>>, Error> {
        let mut buf = self.input_buffer();
        let n = unsafe { self.receive(&mut buf, within.as_millis().max(1) as u32)? };
        Ok(n.map(|n| {
            buf.truncate(n);
            buf
        }))
    }

    /// `report` filled up to the collection's output report size.
    fn padded(&self, report: &[u8]) -> Vec<u8> {
        let mut out = report.to_vec();
        out.resize(self.out_len.max(report.len()), 0);
        out
    }

    fn input_buffer(&self) -> Vec<u8> {
        vec![0u8; self.in_len.max(2)]
    }

    /// One plain write, waited for at most `ms`; None when it did not finish in time.
    unsafe fn send(&self, out: &[u8], ms: u32) -> Result<Option<usize>, Error> {
        self.overlapped(ms, |h, ov| WriteFile(h, Some(out), None, Some(ov)))
    }

    /// One read into `buf`, waited for at most `ms`: the bytes read, None when none came in time.
    unsafe fn receive(&self, buf: &mut [u8], ms: u32) -> Result<Option<usize>, Error> {
        self.overlapped(ms, |h, ov| ReadFile(h, Some(buf), None, Some(ov)))
    }

    /// One overlapped transfer, waited for at most `ms`: the bytes moved, None when it did not finish in time.
    unsafe fn overlapped(&self, ms: u32, start: impl FnOnce(HANDLE, &mut windows::Win32::System::IO::OVERLAPPED) -> windows::core::Result<()>) -> Result<Option<usize>, Error> {
        use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
        use windows::Win32::System::Threading::{ResetEvent, WaitForSingleObject};
        let mut ov = OVERLAPPED { hEvent: self.event, ..Default::default() };
        let mut n = 0u32;
        let _ = ResetEvent(self.event);
        if let Err(e) = start(self.handle, &mut ov) {
            // the error carries the code: GetLastError afterwards may already say something else
            if e.code() != ERROR_IO_PENDING.to_hresult() {
                self.gone.set(true);
                return Err(Error::Io);
            }
        }
        if WaitForSingleObject(self.event, ms) != WAIT_OBJECT_0 {
            let _ = CancelIoEx(self.handle, Some(&ov));
            let _ = GetOverlappedResult(self.handle, &ov, &mut n, true);
            return Ok(None);
        }
        GetOverlappedResult(self.handle, &ov, &mut n, false).map_err(|_| Error::Io)?;
        Ok(Some(n as usize))
    }
}

/// A lithium cell's charge from its voltage: a straight line between the two points of `curve` (falling: mV and
/// the percent there, the first at 100) around `mv`, rounded.
pub fn percent_on(curve: &[(u16, u8)], mv: u16) -> u8 {
    if mv >= curve[0].0 {
        return 100;
    }
    for w in curve.windows(2) {
        let ((hi_mv, hi), (lo_mv, lo)) = (w[0], w[1]);
        if mv >= lo_mv {
            let (span, up) = ((hi_mv - lo_mv) as u32, (mv - lo_mv) as u32);
            return ((lo as u32 * span + (hi - lo) as u32 * up + span / 2) / span) as u8;
        }
    }
    0
}
