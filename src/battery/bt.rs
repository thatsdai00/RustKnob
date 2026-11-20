// Bluetooth devices: the battery level Windows itself keeps for them (what Settings > Bluetooth & devices
// shows, from the Hands-Free profile of a classic device or the Battery Service of an LE one). Nothing is sent
// to the device. The nodes are listed when devices come or go; each look then reads two properties per node.

use std::time::Duration;

use windows::core::GUID;
use windows::Win32::Devices::Properties::*;

use super::{pnp, Kind, Reading, Scan, Source};

const BATTERY: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5), pid: 2 };
const CONNECTED: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x83da6326_97a6_4088_9453_a1923f573b29), pid: 15 };
const CLASS_OF_DEVICE: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x2bd67d8b_8beb_48d5_87e0_6cda3428040a), pid: 10 };

/// Endings Windows gives the service nodes of one headset ("WH-1000XM4 Hands-Free AG").
const SUFFIXES: [&str; 5] = [" Hands-Free AG", " Hands-Free", " Avrcp Transport", " Stereo", " Audio Gateway"];

/// Products that show mouse collections over Bluetooth but are keyboards.
const KEYBOARDS: [(u16, u16); 2] = [(0x3151, 0x4013), (0x19f5, 0x324a)];

/// One physical Bluetooth device (all its nodes share a container id).
struct Device {
    key: String,
    name: String,
    kind: Kind,
    address: u64,      // 0: not known
    mains: Vec<u32>,   // its device nodes (BTHENUM\DEV_, BTHLE\DEV_): their "connected" decides
    battery: Vec<u32>, // the nodes that may carry the level
}

struct Bluetooth {
    devices: Vec<Device>,
}

pub fn driver(scan: &mut Scan, sources: &mut Vec<Box<dyn Source>>) {
    let mut groups: Vec<(GUID, Device)> = Vec::new();
    for id in ["BTHENUM", "BTHLE", "BTHLEDEVICE"].into_iter().flat_map(pnp::ids) {
        let Some(node) = pnp::locate(&id) else { continue };
        let container = pnp::guid(node, &DEVPKEY_Device_ContainerId).unwrap_or_default();
        let at = groups.iter().position(|g| g.0 == container).unwrap_or_else(|| {
            groups.push((container, Device { key: format!("bt:{container:?}"), name: String::new(), kind: Kind::Other, address: 0, mains: Vec::new(), battery: Vec::new() }));
            groups.len() - 1
        });
        let d = &mut groups[at].1;
        let name = pnp::string(node, &DEVPKEY_Device_FriendlyName).or_else(|| pnp::string(node, &DEVPKEY_Device_DeviceDesc)).unwrap_or_default();
        if id.starts_with("BTHENUM\\DEV_") || id.starts_with("BTHLE\\DEV_") {
            d.mains.push(node);
            if !name.is_empty() {
                d.name = name.clone();
            }
            d.address = id.split_once("\\DEV_").and_then(|(_, a)| a.get(..12)).and_then(|a| u64::from_str_radix(a, 16).ok()).unwrap_or(d.address);
            if let Some(kind) = pnp::uint(node, &CLASS_OF_DEVICE).and_then(kind_of_class) {
                d.kind = kind;
            } else if d.kind == Kind::Other {
                d.kind = pnp::string(node, &DEVPKEY_DeviceContainer_Category).map_or(Kind::Other, |c| kind_of_category(&c));
            }
        }
        if pnp::byte(node, &BATTERY).is_some() {
            d.battery.push(node);
            if d.name.is_empty() {
                d.name = strip(&name).to_string();
            }
        }
    }
    groups.retain(|g| !g.1.battery.is_empty());
    if groups.is_empty() {
        return;
    }
    // a device a vendor driver already reads over HID (same container) is left to it; one that is still
    // unclaimed gives its kind from its collections, and the generic HID reader leaves it to this one
    for c in &scan.cols {
        let Some(container) = pnp::container_of(&c.path) else { continue };
        let Some(at) = groups.iter().position(|g| g.0 == container) else { continue };
        if scan.is_claimed(c.vid, c.pid) {
            groups.remove(at);
            continue;
        }
        let d = &mut groups[at].1;
        if KEYBOARDS.contains(&(c.vid, c.pid)) {
            d.kind = Kind::Keyboard;
        } else if d.kind == Kind::Other {
            d.kind = Kind::from_collections(&scan.cols, c.vid, c.pid);
        }
    }
    scan.bt_containers.extend(groups.iter().map(|g| g.0));
    let devices = groups
        .into_iter()
        .map(|(_, mut d)| {
            if d.kind == Kind::Other {
                d.kind = Kind::from_name(&d.name);
            }
            if d.name.is_empty() {
                d.name = "Bluetooth device".into();
            }
            d
        })
        .collect();
    sources.push(Box::new(Bluetooth { devices }));
}

impl Source for Bluetooth {
    fn poll(&mut self, out: &mut Vec<Reading>) -> Duration {
        for d in &self.devices {
            let mut r = Reading::new(&d.key, &d.name, d.kind);
            r.bluetooth = true;
            // the level: a node that is connected wins over one that is not (a headset's Hands-Free node
            // reads disconnected while the audio runs over A2DP, its level stays right)
            let mut best: Option<(u8, bool)> = None;
            for &node in &d.battery {
                let Some(level) = pnp::byte(node, &BATTERY).filter(|&l| l <= 100) else { continue };
                if pnp::has_problem(node) {
                    continue;
                }
                let on = pnp::boolean(node, &CONNECTED).unwrap_or(true);
                if best.is_none_or(|(_, was)| on || !was) {
                    best = Some((level, on));
                }
            }
            let mains: Vec<bool> = d.mains.iter().filter_map(|&n| pnp::boolean(n, &CONNECTED)).collect();
            let connected = if mains.is_empty() { best.is_some_and(|b| b.1) } else { mains.contains(&true) };
            out.push(match best {
                Some((level, _)) if connected => {
                    let _ = d.address; // the earbuds' own levels (Fast Pair) are matched by it
                    r.level(level, false)
                }
                _ => r.offline(),
            });
        }
        Duration::from_secs(10)
    }
}

fn strip(name: &str) -> &str {
    SUFFIXES.iter().find_map(|s| name.strip_suffix(s).filter(|n| !n.is_empty())).unwrap_or(name)
}

/// From the Bluetooth class of device: major 2 phone, 4 audio, 5 peripheral (minor: joystick or gamepad,
/// keyboard, pointing device).
fn kind_of_class(cod: u32) -> Option<Kind> {
    let (major, minor) = ((cod >> 8) & 0x1f, (cod >> 2) & 0x3f);
    match major {
        0x02 => Some(Kind::Phone),
        0x04 => Some(Kind::Headset),
        0x05 if matches!(minor & 0x0f, 1 | 2) => Some(Kind::Gamepad),
        0x05 if minor >> 4 == 1 => Some(Kind::Keyboard),
        0x05 if minor >> 4 == 2 => Some(Kind::Mouse),
        _ => None,
    }
}

fn kind_of_category(c: &str) -> Kind {
    match c {
        "Input.Keyboard" => Kind::Keyboard,
        "Input.Mouse" => Kind::Mouse,
        "Input.Gaming" => Kind::Gamepad,
        "Audio.Headphones" => Kind::Headset,
        _ if c == "Communication.Phone" || c.starts_with("Communication.Phone.") => Kind::Phone,
        _ if c.starts_with("Communication.Headset") => Kind::Headset,
        _ => Kind::Other,
    }
}
