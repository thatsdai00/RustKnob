// Any HID device no vendor driver took that declares the standard battery usage: Battery Strength (Generic
// Device Controls 0x06/0x20) or Absolute State Of Charge (Battery System 0x85/0x65). Its report is asked for
// over the control pipe and the value scaled by the range the descriptor declares.

use std::time::Duration;

use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::{CloseHandle, HANDLE};

use super::{pnp, Kind, Reading, Scan, Source, ASLEEP_EVERY};
use crate::hid::HidInfo;

const BATTERY_USAGES: [(u16, u16); 2] = [(0x06, 0x20), (0x85, 0x65)];
const APPLE: u16 = 0x05ac;

struct Generic {
    handle: HANDLE,
    pp: PHIDP_PREPARSED_DATA,
    ty: HIDP_REPORT_TYPE,
    report_id: u8,
    len: usize,
    page: u16,
    usage: u16,
    link: u16,
    range: (i32, i32),
    apple: bool,
    key: String,
    name: String,
    kind: Kind,
}

// the handle and the preparsed data belong to this source alone
unsafe impl Send for Generic {}

impl Drop for Generic {
    fn drop(&mut self) {
        unsafe {
            let _ = HidD_FreePreparsedData(self.pp);
            let _ = CloseHandle(self.handle);
        }
    }
}

pub fn driver(scan: &mut Scan, sources: &mut Vec<Box<dyn Source>>) {
    let mut found: Vec<(HidInfo, Generic)> = Vec::new();
    for c in scan.free() {
        if found.iter().any(|f| f.0.is(c.vid, c.pid))
            || matches!(c.usage_page, 0x84 | 0x85) // power supplies, UPS units
            || scan.cols.iter().any(|o| o.is(c.vid, c.pid) && o.usage_page == 0x0d) // a pen tablet: asking it flipped an axis
            || c.value_caps == (0, 0)
        {
            continue;
        }
        if pnp::container_of(&c.path).is_some_and(|g| scan.bt_containers.contains(&g)) {
            continue; // Windows already keeps its level (bt.rs)
        }
        if let Some(g) = unsafe { open(c, scan) } {
            found.push((c.clone(), g));
        }
    }
    for (c, g) in found {
        scan.claim(c.vid, c.pid);
        sources.push(Box::new(g));
    }
}

/// The first battery value the collection declares (feature reports before input reports).
unsafe fn open(c: &HidInfo, scan: &Scan) -> Option<Generic> {
    let handle = crate::hid::open(&c.path)?;
    let mut pp = PHIDP_PREPARSED_DATA::default();
    if !HidD_GetPreparsedData(handle, &mut pp).as_bool() {
        let _ = CloseHandle(handle);
        return None;
    }
    for (ty, count, len) in [(HidP_Feature, c.value_caps.0, c.feature_len), (HidP_Input, c.value_caps.1, c.input_len)] {
        if count == 0 || len == 0 {
            continue;
        }
        let mut caps = vec![HIDP_VALUE_CAPS::default(); count as usize];
        let mut n = count;
        if HidP_GetValueCaps(ty, caps.as_mut_ptr(), &mut n, pp).is_err() {
            continue;
        }
        for cap in &caps[..n as usize] {
            let usage = if cap.IsRange.as_bool() { cap.Anonymous.Range.UsageMin } else { cap.Anonymous.NotRange.Usage };
            if !BATTERY_USAGES.contains(&(cap.UsagePage, usage)) {
                continue;
            }
            let name = crate::hid::product(&c.path).unwrap_or_else(|| "Wireless device".into());
            let kind = match Kind::from_collections(&scan.cols, c.vid, c.pid) {
                Kind::Other => Kind::from_name(&name),
                k => k,
            };
            let mut g = Generic {
                handle,
                pp,
                ty,
                report_id: cap.ReportID,
                len: len as usize,
                page: cap.UsagePage,
                usage,
                link: cap.LinkCollection,
                range: (cap.LogicalMin, cap.LogicalMax),
                apple: c.vid == APPLE,
                key: format!("hid:{:04X}:{:04X}", c.vid, c.pid),
                name,
                kind,
            };
            // only one that gives a level now is kept
            if g.percent().is_some() {
                return Some(g);
            }
            std::mem::forget(g); // the handle and data go on to the next try
        }
    }
    let _ = HidD_FreePreparsedData(pp);
    let _ = CloseHandle(handle);
    None
}

impl Generic {
    fn percent(&mut self) -> Option<u8> {
        let mut report = vec![0u8; self.len];
        report[0] = self.report_id;
        unsafe {
            let ok = if self.ty == HidP_Feature {
                HidD_GetFeature(self.handle, report.as_mut_ptr() as *mut _, report.len() as u32)
            } else {
                HidD_GetInputReport(self.handle, report.as_mut_ptr() as *mut _, report.len() as u32)
            };
            if !ok.as_bool() {
                return None;
            }
            let mut value = 0u32;
            if HidP_GetUsageValue(self.ty, self.page, self.link, self.usage, &mut value, self.pp, &report).is_err() {
                return None;
            }
            let (lo, hi) = self.range;
            let v = value as i64;
            if self.apple && v <= 100 {
                Some(v as u8) // Apple declares 0..255 and sends a plain percent
            } else if hi > lo {
                Some(((v - lo as i64) * 100 / (hi as i64 - lo as i64)).clamp(0, 100) as u8)
            } else {
                (v <= 100).then_some(v as u8)
            }
        }
    }
}

impl Source for Generic {
    fn poll(&mut self, out: &mut Vec<Reading>) -> Duration {
        let r = Reading::new(&self.key, &self.name, self.kind);
        out.push(match self.percent() {
            Some(p) => r.level(p, false),
            None => r.asleep(),
        });
        ASLEEP_EVERY
    }
}
