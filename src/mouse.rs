// Gaming mice, straight over HID (no Synapse, no G HUB, no second program): the DPI they are set to, their
// battery, and when they wake up. Reads only. Everything here is shared; only the way of talking to the mouse
// differs per brand:
//
//   Razer Viper V3 Pro           (here)         a 90-byte packet as feature report 0, ported from rustdriver/razer
//   Logitech PRO X SUPERLIGHT 2  (logitech.rs)  HID++ 2.0 reports on the receiver's vendor collection
//   ATK A9 Plus                  (atk.rs)       ATK V HUB reports (two families: Compx, BitMouse)
//   ASUS ROG Gladius III         (asus.rs)      Armoury Crate's 64-byte reports on interface 0
//   Logitech G435 headset        (g435.rs)      its LIGHTSPEED dongle's own reports (battery only, no DPI)
//
// Razer packet (report id first): [1] status 0 new, 1 busy, 2 ok, 3 failed, 4 no answer (asleep / out of
// range), 5 not supported; [2] transaction id 0x1F; [6] argument size; [7] class; [8] id (bit 7: get);
// [9..89] arguments; [89] XOR of [3..89]. It is sent, then read back until the status stops saying "busy".
//
// A thread of its own owns the handle. With no receiver or cable present it sleeps until Windows reports a
// device change; while the mouse sleeps it asks once every 1.5 s; while it is awake it reads the DPI every
// 400 ms (so a press of the DPI button shows quickly), the charger every 3 s and the battery once a minute.
// The first device found is the one followed; any other supported one that is plugged in has its charger looked
// at every few seconds and its battery once a minute too, shows in a row of its own under the cards (see
// `others`), and gets a card of its own when it goes on or off the charger or runs low (EV_OTHER).
//
// The pictures are not in the exe: the first time a mouse shows up, its picture comes from the repository on
// GitHub and is kept in %LOCALAPPDATA%\RustKnob\pictures (offline, the card shows without it).

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::gfx::FClamp;
pub use crate::hid::{hid_find, percent_on, Error, HidInfo, HidIo};
use crate::hid::open;
use crate::asus::Asus;
use crate::atk::Atk;
use crate::g435::G435;
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
    G435, // a headset: no DPI, only on/off and its battery
    GladiusIII,
}

impl Model {
    pub fn name(self) -> &'static str {
        match self {
            Model::ViperV3Pro => "Viper V3 Pro",
            Model::Superlight2 => "Superlight 2",
            Model::AtkA9Plus => "ATK A9 Plus",
            Model::G435 => "G435",
            Model::GladiusIII => "ROG Gladius III",
        }
    }

    pub fn is_headset(self) -> bool {
        self == Model::G435
    }

    /// Its picture's file name, in the repository's assets folder and in the local copy.
    fn picture_file(self) -> &'static str {
        match self {
            Model::ViperV3Pro => "viper-side.png",
            Model::Superlight2 => "superlight2-side.png",
            Model::AtkA9Plus => "atk-a9-side.png",
            Model::G435 => "g435-side.png",
            Model::GladiusIII => "gladius3-side.png",
        }
    }
}

/// Another plugged-in mouse: only its battery.
#[derive(Clone, Copy, PartialEq)]
pub struct Other {
    pub model: Model,
    pub battery: Option<u8>,
    pub charging: bool,
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
pub const EV_OTHER: usize = 6; // another plugged-in device: lparam its Model | why << 8 (the OTHER_* below)
pub const EV_OTHERS: usize = 7; // the other devices' list or one of their levels changed (nothing to show)
pub const OTHER_CHARGING: isize = 0;
pub const OTHER_UNPLUGGED: isize = 1;
pub const OTHER_LOW: isize = 2;
pub const OTHER_CONNECTED: isize = 3; // it woke up (was switched on)
pub const OTHER_ASKED: isize = 4; // its battery icon was clicked (island.rs)

static STATUS: Mutex<Option<Status>> = Mutex::new(None);
static OTHERS: Mutex<Vec<Other>> = Mutex::new(Vec::new());
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

/// The other plugged-in mice that answered at their last look (not the one `status` is about).
pub fn others() -> Vec<Other> {
    OTHERS.lock().map(|o| o.clone()).unwrap_or_default()
}

fn set_others(o: Vec<Other>) {
    if let Ok(mut g) = OTHERS.lock() {
        if *g != o {
            *g = o;
            post(EV_OTHERS);
        }
    }
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
    post_with(ev, 0);
}

fn post_with(ev: usize, lp: isize) {
    unsafe {
        let _ = PostMessageW(HWND(TARGET.load(Ordering::Relaxed) as _), MSG.load(Ordering::Relaxed) as u32, WPARAM(ev), LPARAM(lp));
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
    let Ok(c) = CONTROL.0.lock() else { return false };
    let waiting = |c: &mut Control| c.on && !c.rescan;
    let c = match d {
        Some(d) => CONTROL.1.wait_timeout_while(c, d, waiting).ok().map(|r| r.0),
        None => CONTROL.1.wait_while(c, waiting).ok(),
    };
    c.is_some_and(|c| c.on)
}

fn take_rescan() -> bool {
    CONTROL.0.lock().map(|mut c| std::mem::take(&mut c.rescan)).unwrap_or(false)
}

fn run() {
    loop {
        // off: sleep until switched on
        {
            let Ok(c) = CONTROL.0.lock() else { return };
            if CONTROL.1.wait_while(c, |c| !c.on).is_err() {
                return;
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
        set_others(Vec::new());
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
    let mut others = Vec::new(); // every other plugged-in device (one per brand: the followed one's brand is left out)
    refresh_others(&mut others, m.model);
    let mut others_at: Option<Instant> = None; // when their batteries were last read (None: now)
    let mut others_charger_at = Instant::now();
    loop {
        if others_at.is_none_or(|at| at.elapsed() >= BATTERY_EVERY) {
            others_at = Some(Instant::now());
            others_charger_at = Instant::now();
            read_others(&mut others, true);
        } else if others_charger_at.elapsed() >= CHARGING_EVERY {
            others_charger_at = Instant::now();
            read_others(&mut others, false);
        }
        match m.dpi() {
            Ok(dpi) => match now.as_mut() {
                None => {
                    // woke up (or found awake): everything once
                    quiet = 0;
                    let (stage, stages) = m.stages().unwrap_or((0, 0));
                    let (battery, charging) = m.battery().unwrap_or((None, false));
                    let s = Status { model: m.model, dpi, stage, stages, battery, charging };
                    battery_at = Instant::now();
                    fetch_picture(s.model); // fetched and decoded here, not on the UI thread when the card first draws
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
                        let b = m.percent_or(s.battery);
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
            if Device::preferred().map(|p| p.1).as_ref() != Some(&m.path) {
                if now.is_some() {
                    post(EV_GONE);
                }
                return;
            }
            // the same device: another one may have come or gone (those still there keep what they said:
            // the G435 tells its charger from a change since the last look)
            refresh_others(&mut others, m.model);
        }
    }
}



/// Another plugged-in device, and what it said last (None: asleep, or not looked at yet).
struct Watched {
    device: Device,
    last: Option<Other>,
}

/// After a device change (and at first): drops the others that are gone, opens the new ones, keeps the rest as
/// they are.
fn refresh_others(devices: &mut Vec<Watched>, followed: Model) {
    let present: Vec<(Model, Vec<u16>)> = Device::present().into_iter().filter(|(model, _)| *model != followed).collect();
    devices.retain(|w| present.iter().any(|(_, p)| *p == w.device.path));
    for p in present {
        if !devices.iter().any(|w| w.device.path == p.1) {
            if let Some(device) = Device::open(p) {
                devices.push(Watched { device, last: None });
            }
        }
    }
    set_others(devices.iter().filter_map(|w| w.last).collect());
}

/// 0 fine, 1 low, 2 nearly empty (as the cards count it).
fn level(o: &Other) -> u8 {
    match o.battery {
        Some(p) if !o.charging && p <= 5 => 2,
        Some(p) if !o.charging && p < 25 => 1,
        _ => 0,
    }
}

/// Reads the other devices: `full` wakes and batteries, else only the charger of those awake at the last look.
/// One that went on the charger or got lower gets a card (EV_OTHER); one that is gone (unplugged) is dropped.
fn read_others(devices: &mut Vec<Watched>, full: bool) {
    devices.retain_mut(|w| {
        let now = if full {
            match w.device.look() {
                Ok(n) => n,
                Err(_) => return false,
            }
        } else {
            let Some(last) = w.last else {
                // asleep (or switched off) at the last look: awake now? then everything, and a card
                return match w.device.look() {
                    Ok(Some(n)) => {
                        w.last = Some(n);
                        fetch_picture(n.model);
                        let why = if level(&n) > 0 { OTHER_LOW } else { OTHER_CONNECTED };
                        post_with(EV_OTHER, n.model as isize | why << 8);
                        true
                    }
                    Ok(None) => true,
                    Err(_) => false,
                };
            };
            match w.device.charging() {
                // the charger came or went: the level now too
                Ok(c) if c != last.charging => Some(Other { charging: c, battery: w.device.percent_or(last.battery), ..last }),
                Ok(_) => Some(last),
                Err(Error::Asleep) => None,
                Err(Error::Io) => return false,
            }
        };
        if let (Some(n), Some(l)) = (now, w.last) {
            let why = if n.charging && !l.charging {
                Some(OTHER_CHARGING)
            } else if level(&n) > level(&l) {
                Some(OTHER_LOW) // off the charger and low at once: the warning says more
            } else if l.charging && !n.charging {
                Some(OTHER_UNPLUGGED)
            } else {
                None
            };
            if let Some(why) = why {
                fetch_picture(n.model);
                post_with(EV_OTHER, n.model as isize | why << 8);
            }
        }
        if let (Some(n), None) = (now, w.last) {
            fetch_picture(n.model); // first seen (at start, too): its card may come from a click on its tray icon
        }
        w.last = now;
        true
    });
    set_others(devices.iter().filter_map(|w| w.last).collect());
}

/// What the thread asks a device, whichever brand: each brand's file speaks its own protocol.
pub trait Brand {
    /// The DPI (0 when it is not told); answering at all means it is awake.
    fn dpi(&mut self) -> Result<u16, Error>;

    /// (active stage, how many); one that does not tell its stages has 0 of them (no markers are drawn).
    fn stages(&mut self) -> Result<(usize, usize), Error> {
        Ok((0, 0))
    }

    /// (percent, on the charger).
    fn battery(&mut self) -> Result<(Option<u8>, bool), Error>;

    fn charging(&mut self) -> Result<bool, Error> {
        self.battery().map(|b| b.1)
    }
}

/// The mouse, whichever brand: all the thread needs from it.
struct Device {
    model: Model,
    path: Vec<u16>,
    brand: Box<dyn Brand>,
}

impl std::ops::Deref for Device {
    type Target = dyn Brand;
    fn deref(&self) -> &Self::Target {
        &*self.brand
    }
}

impl std::ops::DerefMut for Device {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.brand
    }
}

/// How a brand finds the path to talk to (each prefers its cable to its receiver).
type Find = fn() -> Option<Vec<u16>>;

/// Every brand, in the order they are looked for.
const BRANDS: [(Model, Find); 5] = [
    (Model::ViperV3Pro, Razer::preferred),
    (Model::Superlight2, Logi::preferred),
    (Model::AtkA9Plus, Atk::preferred),
    (Model::GladiusIII, Asus::preferred),
    (Model::G435, G435::preferred),
];

impl Device {
    /// The model and path to talk to: a Razer first, else a Logitech, and so on down BRANDS.
    fn preferred() -> Option<(Model, Vec<u16>)> {
        BRANDS.iter().find_map(|&(model, find)| find().map(|p| (model, p)))
    }

    /// Every brand's mouse that is plugged in, in the same order.
    fn present() -> Vec<(Model, Vec<u16>)> {
        BRANDS.iter().filter_map(|&(model, find)| find().map(|p| (model, p))).collect()
    }

    fn open((model, path): (Model, Vec<u16>)) -> Option<Device> {
        let brand: Box<dyn Brand> = match model {
            Model::ViperV3Pro => Box::new(Razer::open(&path)?),
            Model::Superlight2 => Box::new(Logi::open(&path)?),
            Model::AtkA9Plus => Box::new(Atk::open(&path)?),
            Model::GladiusIII => Box::new(Asus::open(&path)?),
            Model::G435 => Box::new(G435::open(&path)?),
        };
        Some(Device { model, path, brand })
    }

    /// Awake (then with its battery: Some), asleep (None) or gone (Err).
    fn look(&mut self) -> Result<Option<Other>, Error> {
        match self.dpi() {
            Ok(_) => {
                let (battery, charging) = self.battery().unwrap_or((None, false));
                Ok(Some(Other { model: self.model, battery, charging }))
            }
            Err(Error::Asleep) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The percent now, or `last` when it does not tell.
    fn percent_or(&mut self, last: Option<u8>) -> Option<u8> {
        self.battery().ok().and_then(|b| b.0).or(last)
    }
}

struct Razer {
    handle: HANDLE,
}

impl Drop for Razer {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl Razer {
    fn open(path: &[u16]) -> Option<Razer> {
        Some(Razer { handle: unsafe { open(path)? } })
    }

    /// The configuration collection (a 91-byte feature report): the cable's when it is plugged in, else the
    /// receiver's.
    fn preferred() -> Option<Vec<u16>> {
        hid_find(VID, &[PID_WIRED], &|d| (d.pid == PID_WIRED || d.pid == PID_WIRELESS) && d.feature_len as usize == REPORT_LEN)
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
                    3 => break,                         // failed: send again
                    4 | 5 => return Err(Error::Asleep), // no answer; 5, not supported: treated as no answer
                    _ => continue,                      // new / busy
                }
            }
        }
        Err(Error::Asleep)
    }
}

impl Brand for Razer {
    fn dpi(&mut self) -> Result<u16, Error> {
        let a = self.cmd(0x04, 0x85, 0x07, &[VARSTORE])?;
        Ok(u16::from_be_bytes([a[1], a[2]]))
    }

    fn stages(&mut self) -> Result<(usize, usize), Error> {
        let a = self.cmd(0x04, 0x86, 0x26, &[VARSTORE])?;
        let n = (a[2] as usize).min(STAGES_MAX);
        let active = (0..n).find(|i| a[3 + i * 7] == a[1]).unwrap_or(0);
        Ok((active, n))
    }

    /// The percent from the mouse's 0..255 scale, then the charger.
    fn battery(&mut self) -> Result<(Option<u8>, bool), Error> {
        let a = self.cmd(0x07, 0x80, 0x02, &[])?;
        Ok((Some(((a[1] as u32 * 100 + 127) / 255) as u8), self.charging().unwrap_or(false)))
    }

    fn charging(&mut self) -> Result<bool, Error> {
        Ok(self.cmd(0x07, 0x84, 0x02, &[])?[1] == 1)
    }
}

// ---- the picture of the mouse ----------------------------------------------

/// A small premultiplied picture, with halved copies of it for small sizes on screen.
pub struct Picture {
    levels: Vec<Level>, // full size first
    pub dark: bool,     // mostly dark (a black headset): lifted a little on the dark theme, or it would vanish
}

struct Level {
    w: usize,
    h: usize,
    px: Vec<[u8; 4]>, // premultiplied r, g, b, a
}

impl Picture {
    fn new(w: usize, h: usize, px: Vec<[u8; 4]>) -> Picture {
        let mut levels = vec![Level { w, h, px }];
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
        // average lightness of the covered pixels
        let (mut sum, mut cover) = (0f32, 0f32);
        for p in &levels[0].px {
            sum += (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0;
            cover += p[3] as f32;
        }
        let dark = cover > 0.0 && sum / cover < 0.3;
        Picture { levels, dark }
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
        let x = (u * l.w as f32 - 0.5).fclamp(0.0, l.w as f32 - 1.001);
        let y = (v * l.h as f32 - 0.5).fclamp(0.0, l.h as f32 - 1.001);
        let (x0, y0) = (x as usize, y as usize);
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let at = |xx: usize, yy: usize| l.px[yy * l.w + xx];
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
        let f = |v: u8| v as f32 / 255.0;
        std::array::from_fn(|k| (f(a[k]) * (1.0 - fx) + f(b[k]) * fx) * (1.0 - fy) + (f(c[k]) * (1.0 - fx) + f(d[k]) * fx) * fy)
    }
}

static PICTURES: [OnceLock<Picture>; 5] = [const { OnceLock::new() }; 5];

/// The mouse from the side, once `load_picture` has it (no file or network work here: the UI asks each frame).
pub fn picture(model: Model) -> Option<&'static Picture> {
    PICTURES[model as usize].get()
}

/// Gets the picture ready: from the local copy, else from GitHub (kept for next time). Called off the UI
/// thread; a failed download is tried again the next time the mouse wakes up.
pub fn load_picture(model: Model) {
    let cell = &PICTURES[model as usize];
    if cell.get().is_some() {
        return;
    }
    if let Some(p) = find_picture(model) {
        let _ = cell.set(p);
    }
}

static LOADING: [AtomicBool; 5] = [const { AtomicBool::new(false) }; 5];

/// `load_picture` on a thread of its own, so a slow or hanging download never holds up the device's cards.
/// The copy on disk is ready well within the short wait, so the first card still shows it.
fn fetch_picture(model: Model) {
    let loading = &LOADING[model as usize];
    if picture(model).is_some() || loading.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(move || {
        load_picture(model);
        LOADING[model as usize].store(false, Ordering::Relaxed);
    });
    for _ in 0..20 {
        if !loading.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The decoder is WinRT: a thread of its own, so this thread's COM state does not matter.
fn decoded(png: &[u8]) -> Option<Picture> {
    std::thread::scope(|scope| scope.spawn(|| decode(png)).join().ok().flatten())
}

/// The README pictures are drawn from the project's own assets.
#[cfg(feature = "demo")]
fn find_picture(model: Model) -> Option<Picture> {
    decoded(&std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join(model.picture_file())).ok()?)
}

#[cfg(not(feature = "demo"))]
fn find_picture(model: Model) -> Option<Picture> {
    let dir = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty())?).join("RustKnob").join("pictures");
    let file = dir.join(model.picture_file());
    if let Some(p) = std::fs::read(&file).ok().and_then(|png| decoded(&png)) {
        return Some(p);
    }
    let url = format!("https://raw.githubusercontent.com/{}/main/assets/{}", crate::update::REPO, model.picture_file());
    let png = crate::update::get(&url, 1 << 20)?;
    let p = decoded(&png)?; // kept only when it really is a picture: a broken file would never be fetched again
    // written whole, then renamed: a half-written file is never taken for the picture
    let part = file.with_extension("part");
    if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&part, &png).is_ok() && std::fs::rename(&part, &file).is_err() {
        let _ = std::fs::remove_file(&part);
    }
    Some(p)
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
    Some(Picture::new(w, h, bytes.as_chunks::<4>().0.iter().map(|&[b, g, r, a]| [r, g, b, a]).collect()))
}

#[cfg(test)]
mod tests {
    /// Reads the real mouse (nothing is written). `cargo test razer -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn read_mouse() {
        let mut m = super::Device::preferred().and_then(super::Device::open).expect("no mouse found");
        println!("model {:?}", m.model);
        let t = std::time::Instant::now();
        println!("dpi {:?}", m.dpi());
        println!("stages {:?}", m.stages());
        println!("battery {:?}", m.battery());
        println!("{:?} for 4 reads", t.elapsed());
        super::load_picture(super::Model::ViperV3Pro);
        super::load_picture(super::Model::Superlight2);
        let p = super::picture(super::Model::ViperV3Pro).expect("picture");
        let q = super::picture(super::Model::Superlight2).expect("superlight picture");
        println!("superlight aspect {}", q.aspect());
        println!("picture aspect {} centre {:?}", p.aspect(), p.sample(0.5, 0.5, 40.0));
    }
}
