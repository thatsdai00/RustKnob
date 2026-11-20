// The battery of every wireless device in reach, whatever its brand: mice, keyboards, headsets, controllers,
// earbuds. One thread does it all and sleeps in between:
//
//   - The HID collections are listed only when Windows says a device came or went (never on a timer), and each
//     driver (one file per protocol family, in `DRIVERS` order) claims the ones it speaks to. A claimed product is
//     left alone by every later driver, so the generic ways come last: the battery Windows itself keeps for
//     Bluetooth devices (bt.rs) and the standard HID battery usage (generic.rs).
//   - Each source says when it wants to be asked again: a small read every few seconds while a device is awake
//     (so a charger plugged in shows quickly), rarely while it sleeps or for devices that must not be asked
//     often. Between two of those the thread waits on a condition variable: no CPU at all.
//   - A device that stops answering keeps its last level, dimmed, for its driver's grace time; one its receiver
//     says is switched off goes at once.
//
// The mice the cards follow (mouse.rs: their DPI, their pictures) are left to mouse.rs; `devices` merges both.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::hid::HidInfo;

mod bt;
mod catalog;
mod generic;
mod pnp;
mod script;

/// What a device is, for its icon.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Kind {
    Mouse,
    Keyboard,
    Headset,
    Gamepad,
    Phone,
    #[default]
    Other,
}

impl Kind {
    /// From a product name, when nothing better tells.
    pub fn from_name(name: &str) -> Kind {
        let n = name.to_ascii_lowercase();
        let any = |words: &[&str]| words.iter().any(|w| n.contains(w));
        if any(&["mouse", "mice", "trackball"]) {
            Kind::Mouse
        } else if any(&["keyboard", "keys"]) {
            Kind::Keyboard
        } else if any(&["controller", "gamepad", "xbox", "dualsense", "dualshock", "joy-con"]) {
            Kind::Gamepad
        } else if any(&["head", "buds", "pods", "ear", "wh-", "wf-", "audio", "speaker", "arctis", "cloud", "void", "virtuoso"]) {
            Kind::Headset
        } else {
            Kind::Other
        }
    }

    /// From the top-level usages of a product's collections: a mouse before a gamepad before a keyboard.
    pub fn from_collections(cols: &[HidInfo], vid: u16, pid: u16) -> Kind {
        let has = |u: u16| cols.iter().any(|c| c.is(vid, pid) && c.usage_is(0x01, u));
        if has(0x02) {
            Kind::Mouse
        } else if has(0x04) || has(0x05) {
            Kind::Gamepad
        } else if has(0x06) {
            Kind::Keyboard
        } else {
            Kind::Other
        }
    }
}

/// One device's battery, as shown.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Device {
    pub key: String, // stable identity: the same device keeps it across reads and transports
    pub name: String,
    pub kind: Kind,
    pub bluetooth: bool,
    pub percent: Option<u8>,
    pub charging: bool,
    pub full: bool, // charged and still on the charger
    pub dim: bool,  // not answering now: its last level
}

/// What a source read from one device.
#[derive(Clone, PartialEq, Debug)]
pub struct Reading {
    pub key: String,
    pub name: String,
    pub kind: Kind,
    pub bluetooth: bool,
    pub percent: Option<u8>,
    pub charging: bool,
    pub full: bool,
    pub state: State,
    pub grace: Duration, // how long the last level stays (dimmed) once it stops answering
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    Awake,   // it answered
    Asleep,  // no answer now (asleep, out of range): keep the last level for `grace`
    Offline, // its receiver says it is switched off: gone at once
}

impl Reading {
    pub fn new(key: impl Into<String>, name: impl Into<String>, kind: Kind) -> Reading {
        Reading { key: key.into(), name: name.into(), kind, bluetooth: false, percent: None, charging: false, full: false, state: State::Awake, grace: GRACE }
    }

    pub fn level(mut self, percent: u8, charging: bool) -> Reading {
        self.percent = Some(percent.min(100));
        self.charging = charging;
        self
    }

    pub fn asleep(mut self) -> Reading {
        self.state = State::Asleep;
        self
    }

    pub fn offline(mut self) -> Reading {
        self.state = State::Offline;
        self
    }
}

/// How long a sleeping device keeps its last level unless its driver says otherwise.
pub const GRACE: Duration = Duration::from_secs(600);
/// How often an awake device is asked (one small read: a charger plugged in shows within this).
pub const AWAKE_EVERY: Duration = Duration::from_secs(10);
/// How often a sleeping or silent one is asked.
pub const ASLEEP_EVERY: Duration = Duration::from_secs(30);

/// One thing that reads batteries: a device, a receiver with several devices behind it, or a whole way of
/// reading (Bluetooth). `poll` adds what it read to `out` and says when to ask again.
pub trait Source: Send {
    fn poll(&mut self, out: &mut Vec<Reading>) -> Duration;
}

/// What drivers see while they look for their devices: every present collection, and which products an
/// earlier driver already took.
pub struct Scan {
    pub cols: Vec<HidInfo>,
    claimed: Vec<(u16, u16)>,
    pub bt_containers: Vec<windows::core::GUID>, // devices whose level Windows keeps (bt.rs reads those)
}

impl Scan {
    /// Collections of products nobody claimed yet.
    pub fn free(&self) -> impl Iterator<Item = &HidInfo> {
        self.cols.iter().filter(|c| !self.claimed.contains(&(c.vid, c.pid)))
    }

    /// The first free collection of (vid, pid) that `ours` takes.
    pub fn find(&self, vid: u16, pid: u16, ours: impl Fn(&HidInfo) -> bool) -> Option<HidInfo> {
        self.free().find(|c| c.is(vid, pid) && ours(c)).cloned()
    }

    pub fn claim(&mut self, vid: u16, pid: u16) {
        if !self.claimed.contains(&(vid, pid)) {
            self.claimed.push((vid, pid));
        }
    }

    pub fn is_claimed(&self, vid: u16, pid: u16) -> bool {
        self.claimed.contains(&(vid, pid))
    }

    /// The products (vid, pid) of vendor `vid` with a free collection, each once.
    pub fn products(&self, vid: u16) -> Vec<u16> {
        let mut out: Vec<u16> = Vec::new();
        for c in self.free().filter(|c| c.vid == vid) {
            if !out.contains(&c.pid) {
                out.push(c.pid);
            }
        }
        out
    }
}

/// A driver: looks through the scan, claims what it speaks to, and returns a source for each.
type Driver = fn(&mut Scan, &mut Vec<Box<dyn Source>>);

/// Every driver, in the order they get to claim (vendor protocols first, the generic ways last).
const DRIVERS: &[Driver] = &[catalog::driver, bt::driver, generic::driver];

// ---- the thread ----------------------------------------------------------------

struct Control {
    on: bool,
    rescan: bool,
    started: bool,
    skip: Vec<(u16, u16)>, // products mouse.rs follows
}

static CONTROL: (Mutex<Control>, Condvar) = (Mutex::new(Control { on: false, rescan: false, started: false, skip: Vec::new() }), Condvar::new());
static SHOWN: Mutex<Vec<Device>> = Mutex::new(Vec::new());
static TARGET: AtomicIsize = AtomicIsize::new(0);
static MSG: AtomicIsize = AtomicIsize::new(0);

/// Turns it on or off; `msg` is posted to `hwnd` whenever the list or a level changes. `skip`: the products
/// mouse.rs already reads.
pub fn set_enabled(on: bool, hwnd: isize, msg: u32, skip: Vec<(u16, u16)>) {
    TARGET.store(hwnd, Ordering::Relaxed);
    MSG.store(msg as isize, Ordering::Relaxed);
    let Ok(mut c) = CONTROL.0.lock() else { return };
    (c.on, c.rescan, c.skip) = (on, true, skip);
    if on && !c.started {
        c.started = true;
        let _ = std::thread::Builder::new().name("battery".into()).spawn(run);
    }
    CONTROL.1.notify_all();
}

/// Windows reported a device change: list the collections again.
pub fn rescan() {
    if let Ok(mut c) = CONTROL.0.lock() {
        if c.on {
            c.rescan = true;
            CONTROL.1.notify_all();
        }
    }
}

/// Every device with a battery right now (not the ones mouse.rs follows).
pub fn devices() -> Vec<Device> {
    SHOWN.lock().map(|d| d.clone()).unwrap_or_default()
}

fn publish(now: Vec<Device>) {
    let Ok(mut shown) = SHOWN.lock() else { return };
    if *shown != now {
        *shown = now;
        drop(shown);
        unsafe {
            let _ = PostMessageW(HWND(TARGET.load(Ordering::Relaxed) as _), MSG.load(Ordering::Relaxed) as u32, WPARAM(0), LPARAM(0));
        }
    }
}

/// A source and when it is due.
struct Slot {
    source: Box<dyn Source>,
    due: Instant,
}

/// The last reading of a device and when it last answered.
struct Known {
    reading: Reading,
    seen: Instant,
}

fn run() {
    let mut slots: Vec<Slot> = Vec::new();
    let mut known: Vec<Known> = Vec::new();
    loop {
        // off: drop everything and sleep until switched on
        let (rescan, skip) = {
            let Ok(c) = CONTROL.0.lock() else { return };
            let Ok(mut c) = CONTROL.1.wait_while(c, |c| !c.on) else { return };
            (std::mem::take(&mut c.rescan), c.skip.clone())
        };
        if rescan {
            // a plug-in brings a burst of device changes: let it pass, then take them as one
            std::thread::sleep(Duration::from_millis(400));
            if let Ok(mut c) = CONTROL.0.lock() {
                c.rescan = false;
            }
            slots = find_sources(skip).into_iter().map(|source| Slot { source, due: Instant::now() }).collect();
        }

        let now = Instant::now();
        let mut out = Vec::new();
        for slot in slots.iter_mut().filter(|s| s.due <= now) {
            let mut read = Vec::new();
            let next = slot.source.poll(&mut read);
            slot.due = Instant::now() + next;
            out.extend(read);
        }
        remember(&mut known, out);
        publish(shown(&known));

        // until the next source is due, a device change, or switched off
        let next = slots.iter().map(|s| s.due).min().map_or(Duration::from_secs(3600), |d| d.saturating_duration_since(Instant::now()));
        let Ok(c) = CONTROL.0.lock() else { return };
        let Ok((c, _)) = CONTROL.1.wait_timeout_while(c, next, |c| c.on && !c.rescan) else { return };
        if !c.on {
            drop(c);
            slots.clear();
            known.clear();
            publish(Vec::new());
        }
    }
}

/// Lists every HID collection once and lets each driver claim its own.
fn find_sources(skip: Vec<(u16, u16)>) -> Vec<Box<dyn Source>> {
    let mut scan = Scan { cols: unsafe { crate::hid::hid_list("") }, claimed: skip, bt_containers: Vec::new() };
    let mut sources = Vec::new();
    for driver in DRIVERS {
        driver(&mut scan, &mut sources);
    }
    sources
}

/// Takes in fresh readings: an awake one replaces the last, a sleeping one keeps the last level until its
/// grace runs out, an offline one goes.
fn remember(known: &mut Vec<Known>, readings: Vec<Reading>) {
    let now = Instant::now();
    for r in readings {
        let at = known.iter().position(|k| k.reading.key == r.key);
        match (r.state, at) {
            (State::Offline, Some(i)) => {
                known.remove(i);
            }
            (State::Offline, None) => {}
            (State::Awake, Some(i)) => known[i] = Known { reading: r, seen: now },
            (State::Awake, None) => known.push(Known { reading: r, seen: now }),
            (State::Asleep, Some(i)) => known[i].reading.state = State::Asleep,
            (State::Asleep, None) => {}
        }
    }
    known.retain(|k| k.reading.state == State::Awake || now.duration_since(k.seen) < k.reading.grace);
}

fn shown(known: &[Known]) -> Vec<Device> {
    known
        .iter()
        .map(|k| {
            let r = &k.reading;
            Device {
                key: r.key.clone(),
                name: r.name.clone(),
                kind: r.kind,
                bluetooth: r.bluetooth,
                percent: r.percent,
                charging: r.charging,
                full: r.full,
                dim: r.state != State::Awake,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    /// Every device this PC has, read once: `cargo test --release battery -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn read_all() {
        let t = std::time::Instant::now();
        // the repository's devices folder: a test runs from targeteleasedeps, two levels too deep for Store
        let mut sources = super::find_sources(Vec::new());
        println!("{} sources in {:?}", sources.len(), t.elapsed());
        for s in &mut sources {
            let t = std::time::Instant::now();
            let mut out = Vec::new();
            let next = s.poll(&mut out);
            println!("-- poll {:?}, next in {next:?}", t.elapsed());
            for r in out {
                println!("   {:?}", r);
            }
        }
    }
}

/// Percent on a lithium cell's curve: `curve` falling, (millivolts, percent), straight lines in between.
pub fn on_curve(curve: &[(u16, u8)], mv: u16) -> u8 {
    crate::hid::percent_on(curve, mv)
}

#[cfg(test)]
mod raw {
    /// Raw answers of the two devices on this PC: `cargo test --release raw_dump -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn raw_dump() {
        let cols = unsafe { crate::hid::hid_list("") };
        for c in cols.iter().filter(|c| (c.vid == 0x3151 && c.feature_len >= 7 && c.interface == Some(2)) || (c.vid == 0x1532 && c.feature_len == 91)) {
            let Some(mut io) = crate::hid::HidIo::open_feature(c) else { continue };
            println!("{:04X}:{:04X} mi {:?} page {:04X}:{:04X} feat {}", c.vid, c.pid, c.interface, c.usage_page, c.usage, c.feature_len);
            for _ in 0..3 {
                if c.vid == 0x3151 {
                    io.set_feature(&[0, 0xf7]);
                } else {
                    let mut f = [0u8; 91];
                    f[2] = 0x1f;
                    f[6] = 2;
                    f[7] = 7;
                    f[8] = 0x80;
                    f[89] = f[3..89].iter().fold(0, |a, &b| a ^ b);
                    io.set_feature(&f);
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
                let r = io.get_feature(0).unwrap_or_default();
                println!("  {:02X?}", &r[..r.len().min(12)]);
            }
        }
    }
}
