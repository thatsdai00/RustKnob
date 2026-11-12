// Gaming mice, straight over HID (no Synapse, no G HUB, no second program): the DPI they are set to, their
// battery, and when they wake up. Reads only. Everything here is shared; only the way of talking to the mouse
// differs per brand:
//
//   Razer Viper V3 Pro           (here)         a 90-byte packet as feature report 0, ported from rustdriver/razer
//   Logitech PRO X SUPERLIGHT 2  (logitech.rs)  HID++ 2.0 reports on the receiver's vendor collection
//   ATK A9 Plus                  (atk.rs)       ATK V HUB reports (two families: Compx, BitMouse)
//
// Razer packet (report id first): [1] status 0 new, 1 busy, 2 ok, 3 failed, 4 no answer (asleep / out of
// range), 5 not supported; [2] transaction id 0x1F; [6] argument size; [7] class; [8] id (bit 7: get);
// [9..89] arguments; [89] XOR of [3..89]. It is sent, then read back until the status stops saying "busy".
//
// A thread of its own owns the handle. With no receiver or cable present it sleeps until Windows reports a
// device change; while the mouse sleeps it asks once every 1.5 s; while it is awake it reads the DPI every
// 400 ms (so a press of the DPI button shows quickly), the charger every 3 s and the battery once a minute.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::atk::Atk;
use crate::logitech::Logi;

const VID: u16 = 0x1532;
const PID_WIRED: u16 = 0x00c0;
const PID_WIRELESS: u16 = 0x00c1;
const REPORT_LEN: usize = 91; // report id 0 + the 90-byte packet
const VARSTORE: u8 = 0x01;
const STAGES_MAX: usize = 5;

const AWAKE_EVERY: Duration = Duration::from_millis(400);
const ASLEEP_EVERY: Duration = Duration::from_millis(1500);
const ASLEEP_LONG_EVERY: Duration = Duration::from_secs(4); // after a long sleep (overnight): less often
const ASLEEP_LONG: Duration = Duration::from_secs(300);
const BATTERY_EVERY: Duration = Duration::from_secs(60);
const CHARGING_EVERY: Duration = Duration::from_secs(3); // is it on the charger (now)? One small read.
const QUIET_FOR_SLEEP: u32 = 3; // polls in a row without an answer before it counts as asleep (radio hiccups)

/// Which mouse it is.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum Model {
    #[default]
    ViperV3Pro,
    Superlight2,
    AtkA9Plus,
}

impl Model {
    pub fn name(self) -> &'static str {
        match self {
            Model::ViperV3Pro => "Viper V3 Pro",
            Model::Superlight2 => "Superlight 2",
            Model::AtkA9Plus => "ATK A9 Plus",
        }
    }
}

/// What the mouse said last.
#[derive(Clone, Copy, PartialEq, Default)]
pub struct Status {
    pub model: Model,
    pub dpi: u16,
    pub stage: usize,  // 0-based active DPI stage
    pub stages: usize, // how many stages there are
    pub battery: Option<u8>,
    pub charging: bool,
}

/// wparam of the message posted to the window.
pub const EV_DPI: usize = 1; // the DPI (or stage) changed
pub const EV_BATTERY: usize = 2; // battery level or charging changed
pub const EV_WAKE: usize = 3; // the mouse woke up (or was found awake)
pub const EV_SLEEP: usize = 4; // it stopped answering: asleep or out of range
pub const EV_GONE: usize = 5; // receiver / cable unplugged

static STATUS: Mutex<Option<Status>> = Mutex::new(None);
static TARGET: AtomicIsize = AtomicIsize::new(0);
static MSG: AtomicIsize = AtomicIsize::new(0);

struct Control {
    on: bool,
    rescan: bool, // a device came or went: look again
    started: bool,
}
static CONTROL: (Mutex<Control>, Condvar) = (Mutex::new(Control { on: false, rescan: false, started: false }), Condvar::new());

/// The last status, while the mouse is awake.
pub fn status() -> Option<Status> {
    STATUS.lock().ok().and_then(|s| *s)
}

/// Turns the mouse support on or off; `msg` is posted to `hwnd` with an EV_* code.
pub fn set_enabled(on: bool, hwnd: isize, msg: u32) {
    TARGET.store(hwnd, Ordering::Relaxed);
    MSG.store(msg as isize, Ordering::Relaxed);
    let Ok(mut c) = CONTROL.0.lock() else { return };
    c.on = on;
    c.rescan = true;
    if on && !c.started {
        c.started = true;
        let _ = std::thread::Builder::new().name("razer".into()).spawn(run);
    }
    CONTROL.1.notify_all();
}

/// Windows reported a device change (WM_DEVICECHANGE): a receiver or cable may have come or gone.
pub fn rescan() {
    if let Ok(mut c) = CONTROL.0.lock() {
        if c.on {
            c.rescan = true;
            CONTROL.1.notify_all();
        }
    }
}

fn post(ev: usize) {
    unsafe {
        let _ = PostMessageW(HWND(TARGET.load(Ordering::Relaxed) as _), MSG.load(Ordering::Relaxed) as u32, WPARAM(ev), LPARAM(0));
    }
}

fn publish(s: Option<Status>) {
    if let Ok(mut g) = STATUS.lock() {
        *g = s;
    }
}

/// Waits `d` (None: until told), or until the support is switched off or a rescan is asked for.
/// Returns false when it was switched off.
fn wait(d: Option<Duration>) -> bool {
    let Ok(mut c) = CONTROL.0.lock() else { return false };
    let deadline = d.map(|d| Instant::now() + d);
    while c.on && !c.rescan {
        match deadline {
            Some(at) => {
                let now = Instant::now();
                if now >= at {
                    break;
                }
                c = match CONTROL.1.wait_timeout(c, at - now) {
                    Ok((g, _)) => g,
                    Err(_) => return false,
                };
            }
            None => {
                c = match CONTROL.1.wait(c) {
                    Ok(g) => g,
                    Err(_) => return false,
                };
            }
        }
    }
    c.on
}

fn take_rescan() -> bool {
    CONTROL.0.lock().map(|mut c| std::mem::take(&mut c.rescan)).unwrap_or(false)
}

fn run() {
    loop {
        // off: sleep until switched on
        {
            let Ok(mut c) = CONTROL.0.lock() else { return };
            while !c.on {
                c = match CONTROL.1.wait(c) {
                    Ok(g) => g,
                    Err(_) => return,
                };
            }
        }
        settle_rescans();
        let Some(mut mouse) = Device::preferred().and_then(Device::open) else {
            publish(None);
            wait(None); // nothing plugged in: until Windows says something changed
            continue;
        };
        watch(&mut mouse);
        publish(None);
        // it went away (or stopped answering transfers): give it a moment rather than looking again at once
        wait(Some(Duration::from_secs(2)));
    }
}

/// A plug-in brings a burst of device-change messages: let it pass, then take them as one.
fn settle_rescans() {
    take_rescan();
    std::thread::sleep(Duration::from_millis(300));
    take_rescan();
}

/// Follows one plugged-in receiver (or cable) until it goes away or the support is switched off.
fn watch(m: &mut Device) {
    let mut now: Option<Status> = None; // None while asleep
    let (mut battery_at, mut charging_at) = (Instant::now(), Instant::now());
    let (mut quiet, mut asleep_since) = (0u32, Instant::now());
    loop {
        match m.dpi() {
            Ok(dpi) => match now.as_mut() {
                None => {
                    // woke up (or found awake): everything once
                    quiet = 0;
                    let (stage, stages) = m.stages().unwrap_or((0, 0));
                    let (battery, charging) = m.battery().unwrap_or((None, false));
                    let s = Status { model: m.model(), dpi, stage, stages, battery, charging };
                    battery_at = Instant::now();
                    let _ = picture(s.model); // decoded here, not on the UI thread when the card first draws
                    now = Some(s);
                    publish(now);
                    post(EV_WAKE);
                }
                Some(s) => {
                    quiet = 0;
                    if dpi != s.dpi {
                        (s.dpi, (s.stage, s.stages)) = (dpi, m.stages().unwrap_or((s.stage, s.stages)));
                        publish(Some(*s));
                        post(EV_DPI);
                    }
                    // the charger: looked at every few seconds (plugging it in shows a card); the level once a minute,
                    // and at once when the charger comes or goes
                    let mut changed = false;
                    if charging_at.elapsed() >= CHARGING_EVERY {
                        charging_at = Instant::now();
                        let c = m.charging().unwrap_or(s.charging);
                        if c != s.charging {
                            s.charging = c;
                            changed = true;
                            battery_at = Instant::now().checked_sub(BATTERY_EVERY).unwrap_or(battery_at); // the level now too
                        }
                    }
                    if battery_at.elapsed() >= BATTERY_EVERY {
                        battery_at = Instant::now();
                        let b = m.battery().ok().and_then(|b| b.0).or(s.battery);
                        changed |= b != s.battery;
                        s.battery = b;
                    }
                    if changed {
                        publish(Some(*s));
                        post(EV_BATTERY);
                    }
                }
            },
            Err(Error::Asleep) => {
                quiet += 1;
                if quiet >= QUIET_FOR_SLEEP && now.take().is_some() {
                    asleep_since = Instant::now();
                    publish(None);
                    post(EV_SLEEP);
                }
            }
            Err(Error::Io) => {
                // unplugged (or the handle went bad)
                if now.is_some() {
                    post(EV_GONE);
                }
                return;
            }
        }
        let every = match now {
            Some(_) => AWAKE_EVERY,
            None if asleep_since.elapsed() > ASLEEP_LONG => ASLEEP_LONG_EVERY,
            None => ASLEEP_EVERY,
        };
        if !wait(Some(every)) {
            return;
        }
        // a device came or went: is this still the way to the mouse (the cable wins when it is plugged in)?
        if take_rescan() {
            settle_rescans();
            if Device::preferred().map(|p| p.1).as_deref() != Some(m.path()) {
                if now.is_some() {
                    post(EV_GONE);
                }
                return;
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Error {
    Io,     // transfer failed: unplugged
    Asleep, // the receiver answers for the mouse: asleep, off or out of range (or it did not answer at all)
}

/// The mouse, whichever brand: all the thread needs from it.
enum Device {
    Razer(Razer),
    Logitech(Logi),
    Atk(Atk),
}

impl Device {
    /// The model and path to talk to: a Razer first, else a Logitech (each prefers its cable to its receiver).
    fn preferred() -> Option<(Model, Vec<u16>)> {
        Razer::preferred()
            .map(|p| (Model::ViperV3Pro, p))
            .or_else(|| Logi::preferred().map(|p| (Model::Superlight2, p)))
            .or_else(|| Atk::preferred().map(|p| (Model::AtkA9Plus, p)))
    }

    fn open((model, path): (Model, Vec<u16>)) -> Option<Device> {
        match model {
            Model::ViperV3Pro => Razer::open(path).map(Device::Razer),
            Model::Superlight2 => Logi::open(path).map(Device::Logitech),
            Model::AtkA9Plus => Atk::open(path).map(Device::Atk),
        }
    }

    fn model(&self) -> Model {
        match self {
            Device::Razer(_) => Model::ViperV3Pro,
            Device::Logitech(_) => Model::Superlight2,
            Device::Atk(_) => Model::AtkA9Plus,
        }
    }

    fn path(&self) -> &[u16] {
        match self {
            Device::Razer(m) => &m.path,
            Device::Logitech(m) => &m.path,
            Device::Atk(m) => &m.path,
        }
    }

    fn dpi(&mut self) -> Result<u16, Error> {
        match self {
            Device::Razer(m) => m.dpi(),
            Device::Logitech(m) => m.dpi(),
            Device::Atk(m) => m.dpi(),
        }
    }

    /// (active stage, how many); the Logitech does not tell its stages (0 of them: no markers are drawn).
    fn stages(&mut self) -> Result<(usize, usize), Error> {
        match self {
            Device::Razer(m) => m.stages(),
            Device::Logitech(_) => Ok((0, 0)),
            Device::Atk(m) => m.stages(),
        }
    }

    /// (percent, on the charger).
    fn battery(&mut self) -> Result<(Option<u8>, bool), Error> {
        match self {
            Device::Razer(m) => Ok((Some(m.battery()?), m.charging().unwrap_or(false))),
            Device::Logitech(m) => m.battery(),
            Device::Atk(m) => m.battery(),
        }
    }

    fn charging(&mut self) -> Result<bool, Error> {
        match self {
            Device::Razer(m) => m.charging(),
            Device::Logitech(m) => m.battery().map(|b| b.1),
            Device::Atk(m) => m.battery().map(|b| b.1),
        }
    }
}

struct Razer {
    handle: HANDLE,
    path: Vec<u16>,
}

impl Drop for Razer {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl Razer {
    fn open(path: Vec<u16>) -> Option<Razer> {
        let handle = unsafe { open(&path)? };
        Some(Razer { handle, path })
    }

    /// The configuration collection (a 91-byte feature report): the cable's when it is plugged in, else the
    /// receiver's.
    fn preferred() -> Option<Vec<u16>> {
        let all = unsafe { hid_list("vid_1532") };
        let pick = |pid: u16| all.iter().find(|d| d.vid == VID && d.pid == pid && d.feature_len as usize == REPORT_LEN).map(|d| d.path.clone());
        pick(PID_WIRED).or_else(|| pick(PID_WIRELESS))
    }

    fn cmd(&mut self, class: u8, id: u8, size: u8, args: &[u8]) -> Result<[u8; 80], Error> {
        let mut pkt = [0u8; REPORT_LEN];
        pkt[2] = 0x1f;
        pkt[6] = size;
        pkt[7] = class;
        pkt[8] = id;
        pkt[9..9 + args.len().min(80)].copy_from_slice(&args[..args.len().min(80)]);
        pkt[89] = pkt[3..89].iter().fold(0, |c, &x| c ^ x);
        for _attempt in 0..3 {
            if !unsafe { HidD_SetFeature(self.handle, pkt.as_ptr() as *const _, REPORT_LEN as u32) }.as_bool() {
                return Err(Error::Io);
            }
            for i in 0..60 {
                // the first read waits a moment too: straight away it can still hold the previous answer
                std::thread::sleep(Duration::from_millis(if i < 10 { 1 } else { 4 }));
                let mut r = [0u8; REPORT_LEN];
                if !unsafe { HidD_GetFeature(self.handle, r.as_mut_ptr() as *mut _, REPORT_LEN as u32) }.as_bool() {
                    return Err(Error::Io);
                }
                if r[7] != class || r[8] != id {
                    continue; // not the answer to this request yet
                }
                match r[1] {
                    2 => {
                        let mut a = [0u8; 80];
                        a.copy_from_slice(&r[9..89]);
                        return Ok(a);
                    }
                    3 => break, // failed: send again
                    4 => return Err(Error::Asleep),
                    5 => return Err(Error::Asleep), // not supported: treat as no answer
                    _ => continue,                  // new / busy
                }
            }
        }
        Err(Error::Asleep)
    }

    fn dpi(&mut self) -> Result<u16, Error> {
        let a = self.cmd(0x04, 0x85, 0x07, &[VARSTORE])?;
        Ok(u16::from_be_bytes([a[1], a[2]]))
    }

    /// (active stage, how many).
    fn stages(&mut self) -> Result<(usize, usize), Error> {
        let a = self.cmd(0x04, 0x86, 0x26, &[VARSTORE])?;
        let n = (a[2] as usize).min(STAGES_MAX);
        let active = (0..n).find(|i| a[3 + i * 7] == a[1]).unwrap_or(0);
        Ok((active, n))
    }

    /// Percent, from the mouse's 0..255 scale.
    fn battery(&mut self) -> Result<u8, Error> {
        let a = self.cmd(0x07, 0x80, 0x02, &[])?;
        Ok(((a[1] as u32 * 100 + 127) / 255) as u8)
    }

    fn charging(&mut self) -> Result<bool, Error> {
        Ok(self.cmd(0x07, 0x84, 0x02, &[])?[1] == 1)
    }
}

/// Opens a HID collection with no access rights: enough for feature reports and for asking what it is (Windows
/// lets nobody open a mouse collection for reading: the input stack owns it).
unsafe fn open(path: &[u16]) -> Option<HANDLE> {
    CreateFileW(PCWSTR(path.as_ptr()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None).ok().filter(|h| !h.is_invalid())
}

/// One present HID collection of a vendor.
pub struct HidInfo {
    pub path: Vec<u16>,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub feature_len: u16,
    pub input_len: u16,
    pub output_len: u16,
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
unsafe fn probe(path: Vec<u16>) -> Option<HidInfo> {
    let h = open(&path)?;
    let mut attr = HIDD_ATTRIBUTES { Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
    let mut result = None;
    if HidD_GetAttributes(h, &mut attr).as_bool() {
        let mut pp = PHIDP_PREPARSED_DATA::default();
        if HidD_GetPreparsedData(h, &mut pp).as_bool() {
            let mut caps = HIDP_CAPS::default();
            if HidP_GetCaps(pp, &mut caps) == HIDP_STATUS_SUCCESS {
                result = Some(HidInfo {
                    path,
                    vid: attr.VendorID,
                    pid: attr.ProductID,
                    usage_page: caps.UsagePage,
                    usage: caps.Usage,
                    feature_len: caps.FeatureReportByteLength,
                    input_len: caps.InputReportByteLength,
                    output_len: caps.OutputReportByteLength,
                });
            }
            let _ = HidD_FreePreparsedData(pp);
        }
    }
    let _ = CloseHandle(h);
    result
}

// ---- reports with timeouts (Logitech, ATK) -----------------------------------

/// A HID collection opened for output and input reports, every wait bounded (a mouse that is not there must
/// not hang the thread). Razer needs none of this: it talks through feature reports.
pub struct HidIo {
    handle: HANDLE,
    event: HANDLE,
    pub pid: u16,
    out_len: usize, // report sizes of the collection, report id included
    in_len: usize,
}

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
        unsafe {
            let info = probe(path.to_vec())?;
            let handle = CreateFileW(PCWSTR(path.as_ptr()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAG_OVERLAPPED, None).ok().filter(|h| !h.is_invalid())?;
            let Ok(event) = windows::Win32::System::Threading::CreateEventW(None, true, false, PCWSTR::null()) else {
                let _ = CloseHandle(handle);
                return None;
            };
            Some(HidIo { handle, event, pid: info.pid, out_len: info.output_len as usize, in_len: info.input_len as usize })
        }
    }

    /// Sends `report` (its id first) and returns the first input report within `within` that `answer` takes
    /// (it returns None for reports that are something else: notifications, other requests' answers).
    pub fn ask<R>(&mut self, report: &[u8], within: Duration, mut answer: impl FnMut(&[u8]) -> Option<Result<R, Error>>) -> Result<R, Error> {
        let mut out = report.to_vec();
        out.resize(self.out_len.max(report.len()), 0);
        unsafe {
            if !HidD_SetOutputReport(self.handle, out.as_ptr() as *const _, out.len() as u32).as_bool() {
                // some devices only take it as a plain write
                let mut n = 0u32;
                if !self.overlapped(200, |h, ov| WriteFile(h, Some(&out), None, Some(ov)), &mut n)? {
                    return Err(Error::Asleep);
                }
            }
            let until = Instant::now() + within;
            let mut buf = vec![0u8; self.in_len.max(2)];
            loop {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(Error::Asleep);
                }
                let mut n = 0u32;
                if !self.overlapped(left.as_millis() as u32, |h, ov| ReadFile(h, Some(&mut buf), None, Some(ov)), &mut n)? {
                    return Err(Error::Asleep);
                }
                if let Some(r) = answer(&buf[..n as usize]) {
                    return r;
                }
            }
        }
    }

    /// One overlapped transfer, waited for at most `ms`; false when it did not finish in time.
    unsafe fn overlapped(&self, ms: u32, start: impl FnOnce(HANDLE, &mut windows::Win32::System::IO::OVERLAPPED) -> windows::core::Result<()>, n: &mut u32) -> Result<bool, Error> {
        use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
        use windows::Win32::System::Threading::{ResetEvent, WaitForSingleObject};
        let mut ov = OVERLAPPED { hEvent: self.event, ..Default::default() };
        let _ = ResetEvent(self.event);
        if let Err(e) = start(self.handle, &mut ov) {
            // the error carries the code: GetLastError afterwards may already say something else
            if e.code() != ERROR_IO_PENDING.to_hresult() {
                return Err(Error::Io);
            }
        }
        if WaitForSingleObject(self.event, ms) != WAIT_OBJECT_0 {
            let _ = CancelIoEx(self.handle, Some(&ov));
            let _ = GetOverlappedResult(self.handle, &ov, n, true);
            return Ok(false);
        }
        GetOverlappedResult(self.handle, &ov, n, false).map_err(|_| Error::Io)?;
        Ok(true)
    }
}

// ---- the picture of the mouse ----------------------------------------------

/// A small premultiplied picture (0xAARRGGBB), with halved copies of it for small sizes on screen.
pub struct Picture {
    levels: Vec<Level>, // full size first
}

struct Level {
    w: usize,
    h: usize,
    px: Vec<[u8; 4]>, // premultiplied r, g, b, a
}

impl Picture {
    fn new(w: usize, h: usize, argb: &[u32]) -> Picture {
        let ch = |p: u32, shift: u32| ((p >> shift) & 0xFF) as u8;
        let mut levels = vec![Level { w, h, px: argb.iter().map(|&p| [ch(p, 16), ch(p, 8), ch(p, 0), ch(p, 24)]).collect() }];
        while let Some(l) = levels.last().filter(|l| l.h >= 32 && l.w >= 32) {
            let (w, h) = (l.w / 2, l.h / 2);
            let px = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w * 2, i / w * 2);
                    let at = |xx: usize, yy: usize| l.px[yy * l.w + xx];
                    let (a, b, c, d) = (at(x, y), at(x + 1, y), at(x, y + 1), at(x + 1, y + 1));
                    std::array::from_fn(|k| ((a[k] as u16 + b[k] as u16 + c[k] as u16 + d[k] as u16 + 2) / 4) as u8)
                })
                .collect();
            levels.push(Level { w, h, px });
        }
        Picture { levels }
    }

    /// Width / height.
    pub fn aspect(&self) -> f32 {
        let l = &self.levels[0];
        l.w as f32 / l.h as f32
    }

    /// Premultiplied [r, g, b, a] at (u, v) in 0..1, bilinear, from the smallest copy that is still at least
    /// `shown_h` (device pixels) tall.
    pub fn sample(&self, u: f32, v: f32, shown_h: f32) -> [f32; 4] {
        let l = self.levels.iter().rev().find(|l| l.h as f32 >= shown_h).unwrap_or(&self.levels[0]);
        let x = (u * l.w as f32 - 0.5).clamp(0.0, l.w as f32 - 1.001);
        let y = (v * l.h as f32 - 0.5).clamp(0.0, l.h as f32 - 1.001);
        let (x0, y0) = (x as usize, y as usize);
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let at = |xx: usize, yy: usize| l.px[yy * l.w + xx];
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
        let f = |v: u8| v as f32 / 255.0;
        std::array::from_fn(|k| (f(a[k]) * (1.0 - fx) + f(b[k]) * fx) * (1.0 - fy) + (f(c[k]) * (1.0 - fx) + f(d[k]) * fx) * fy)
    }
}

/// The mouse from the side, decoded once per model (on a thread of its own: the decoder is WinRT).
pub fn picture(model: Model) -> Option<&'static Picture> {
    static VIPER: OnceLock<Option<Picture>> = OnceLock::new();
    static SUPERLIGHT: OnceLock<Option<Picture>> = OnceLock::new();
    static ATK: OnceLock<Option<Picture>> = OnceLock::new();
    let (cell, png): (&OnceLock<Option<Picture>>, &'static [u8]) = match model {
        Model::ViperV3Pro => (&VIPER, include_bytes!("../assets/viper-side.png")),
        Model::Superlight2 => (&SUPERLIGHT, include_bytes!("../assets/superlight2-side.png")),
        Model::AtkA9Plus => (&ATK, include_bytes!("../assets/atk-a9-side.png")),
    };
    cell.get_or_init(|| std::thread::spawn(move || decode(png)).join().ok().flatten()).as_ref()
}

fn decode(png: &[u8]) -> Option<Picture> {
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat};
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let stream = InMemoryRandomAccessStream::new().ok()?;
    let writer = DataWriter::CreateDataWriter(&stream).ok()?;
    writer.WriteBytes(png).ok()?;
    writer.StoreAsync().ok()?.get().ok()?;
    writer.DetachStream().ok()?;
    stream.Seek(0).ok()?;
    let decoder = BitmapDecoder::CreateAsync(&stream).ok()?.get().ok()?;
    let bitmap = decoder.GetSoftwareBitmapAsync().ok()?.get().ok()?;
    let bitmap = windows::Graphics::Imaging::SoftwareBitmap::ConvertWithAlpha(&bitmap, BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied).ok()?;
    let (w, h) = (bitmap.PixelWidth().ok()? as usize, bitmap.PixelHeight().ok()? as usize);
    let mut bytes = vec![0u8; w * h * 4];
    let buffer = windows::Storage::Streams::Buffer::Create(bytes.len() as u32).ok()?;
    bitmap.CopyToBuffer(&buffer).ok()?;
    let reader = windows::Storage::Streams::DataReader::FromBuffer(&buffer).ok()?;
    reader.ReadBytes(&mut bytes).ok()?;
    let px: Vec<u32> = bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    Some(Picture::new(w, h, &px))
}

#[cfg(test)]
mod tests {
    /// Reads the real mouse (nothing is written). `cargo test razer -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn read_mouse() {
        let mut m = super::Device::preferred().and_then(super::Device::open).expect("no mouse found");
        println!("model {:?}", m.model());
        let t = std::time::Instant::now();
        println!("dpi {:?}", m.dpi());
        println!("stages {:?}", m.stages());
        println!("battery {:?}", m.battery());
        println!("{:?} for 4 reads", t.elapsed());
        let p = super::picture(super::Model::ViperV3Pro).expect("picture");
        let q = super::picture(super::Model::Superlight2).expect("superlight picture");
        println!("superlight aspect {}", q.aspect());
        println!("picture aspect {} centre {:?}", p.aspect(), p.sample(0.5, 0.5, 40.0));
    }
}
