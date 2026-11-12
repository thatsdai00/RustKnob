// ATK A9 Plus (ATK V HUB mice): battery, charging, DPI and wake-ups, reads only. Driven by the mouse thread in
// mouse.rs like the other brands; only the talking is here. ATK ships the A9 Plus with two different
// protocols, both read from ATK's own web driver (hub.atk.pro, its JavaScript), vendor id 0x373B:
//
// Compx (vendor page 0xFF02, usage 2, the collection whose reports are 17 bytes): output / input report 8, 16 bytes
// after the id (seen on an A9 Plus receiver, 10C9; its 0xFF04 collection refuses every request):
//   [0] command  [1] status (0 ok)  [2..4] EEPROM address (big endian)  [4] length  [5..15] data
//   [15] 0x55 - (8 + sum of [0..15])
//   0x04 battery: data [percent, charging]      0x03 online (receiver): data[0] == 1
//   0x08 EEPROM read: at 0x00 data[2] = stages, data[4] = active stage; at 0x0C + 8 * (stage / 2) two stages
//        as [x lo, y lo, extra, check], x = ((x lo | (extra >> 2 & 3) << 8) + 1) * 50, doubled when extra & 1
//
// BitMouse / NearLink (vendor page 0xFF05, usage 1): output report 8, 63 bytes after the id:
//   [0] sum of [1..63]  [1] 0x72  [2] parameter length  [3] 0  [4] target (1: through the receiver to the
//   mouse)  [5] command  [6] command length  [7..] data;  answers carry the command at [5] (report id first)
//   0x07 battery: data[0] percent   0x0F charging: data[0] == 1   0x81 receiver: a mouse paired (non-zero)
//   0x23 mouse online: 0   0x17 read: address (u16 LE) and length; at 1: [active stage, stages], then 8 bytes
//   per stage from 3: x (u16 LE), y, colour
//
// Compx read on a real A9 Plus receiver (battery, online, stages); BitMouse not tried on a real one. Every value
// is checked before it is shown (a percent over 100, a DPI out of the sensor's range): a wrong reading shows
// nothing rather than nonsense.

use std::time::{Duration, Instant};

use crate::mouse::{Error, HidIo, HidInfo};

const VID: u16 = 0x373b;
const COMPX: [u16; 9] = [0x1115, 0x1067, 0x1068, 0x1069, 0x106a, 0x10c9, 0x10ca, 0x10cb, 0x10cc];
const BITMOUSE: [u16; 32] = [
    0x1224, 0x1263, 0x1300, 0x1337, 0x134f, // the mice; the rest are receivers ("8K", "nano")
    0x1155, 0x11dc, 0x1216, 0x124f, 0x1290, 0x1291, 0x12af, 0x12cd, 0x12d1, 0x12ea, 0x12f5, 0x1302, 0x1315, 0x1332, 0x133e, 0x133f, 0x1340, 0x1341,
    0x1342, 0x1343, 0x1344, 0x1345, 0x1346, 0x1347, 0x1348, 0x134b, 0x1375,
];
const MICE: [u16; 6] = [0x1115, 0x1224, 0x1263, 0x1300, 0x1337, 0x134f]; // the rest are receivers
const ANSWER_WITHIN: Duration = Duration::from_millis(250);
// On the charger the mouse tells a percent lifted by the charger's voltage: it starts from the one before and
// climbs no faster than this (the A9 Plus fills in about an hour and a half).
const PERCENT_PER_MINUTE: f32 = 1.1;

#[derive(Clone, Copy, PartialEq)]
enum Family {
    Compx,
    BitMouse,
}

pub struct Atk {
    io: HidIo,
    pub path: Vec<u16>,
    family: Family,
    receiver: bool,
    last_dpi: u16,                  // the last believable reading: a garbled one shows this instead
    last_stages: (usize, usize),
    last_percent: Option<u8>,       // off the charger
    plugged: Option<(Instant, u8)>, // on the charger: since when, and the percent then
}

impl Atk {
    /// The collection to talk to: a mouse on its cable before a receiver.
    pub fn preferred() -> Option<Vec<u16>> {
        let all = unsafe { crate::mouse::hid_list("vid_373b") };
        let ours = |d: &&HidInfo| d.vid == VID && family(d).is_some();
        all.iter().filter(ours).min_by_key(|d| !MICE.contains(&d.pid)).map(|d| d.path.clone())
    }

    pub fn open(path: Vec<u16>) -> Option<Atk> {
        let io = HidIo::open(&path)?;
        let family = if COMPX.contains(&io.pid) { Family::Compx } else { Family::BitMouse };
        let receiver = !MICE.contains(&io.pid);
        Some(Atk { io, path, family, receiver, last_dpi: 0, last_stages: (0, 0), last_percent: None, plugged: None })
    }

    // ---- Compx -----------------------------------------------------------------

    fn compx(&mut self, command: u8, address: u16, length: u8) -> Result<[u8; 10], Error> {
        let mut r = [0u8; 17];
        r[0] = 8;
        r[1] = command;
        r[3..5].copy_from_slice(&address.to_be_bytes());
        r[5] = length;
        r[16] = 0x55u8.wrapping_sub(r[..16].iter().fold(0u8, |s, &b| s.wrapping_add(b)));
        self.io.ask(&r, ANSWER_WITHIN, |a| {
            // any report id; the answer echoes the command and the address (a late answer to another read of the
            // EEPROM, left waiting in the queue, is not this one)
            (a.len() >= 17 && a[1] == command && a[3..5] == r[3..5]).then(|| {
                if a[2] != 0 {
                    return Err(Error::Asleep); // failed or not supported
                }
                let mut data = [0u8; 10];
                data.copy_from_slice(&a[6..16]);
                Ok(data)
            })
        })
    }

    // ---- BitMouse --------------------------------------------------------------

    fn bitmouse(&mut self, target: u8, command: u8, data: &[u8], parameter_len: u8, command_len: u8) -> Result<Vec<u8>, Error> {
        let mut r = vec![0u8; 64];
        r[0] = 8;
        r[2] = 0x72;
        r[3] = parameter_len;
        r[5] = target;
        r[6] = command;
        r[7] = command_len;
        r[8..8 + data.len()].copy_from_slice(data);
        r[1] = r[2..].iter().fold(0u8, |s, &b| s.wrapping_add(b));
        self.io.ask(&r, ANSWER_WITHIN, |a| {
            (a.len() >= 16 && a[5] == command).then(|| if a[2] == 0xff { Err(Error::Asleep) } else { Ok(a[7..].to_vec()) })
        })
    }

    /// Bytes of the BitMouse's settings at `address`.
    fn bitmouse_read(&mut self, address: u16, length: u8) -> Result<Vec<u8>, Error> {
        let [lo, hi] = address.to_le_bytes();
        let target = u8::from(self.receiver);
        let a = self.bitmouse(target, 0x17, &[lo, hi, length], 10, 13)?;
        Ok(a.get(3..3 + length as usize).map(<[u8]>::to_vec).unwrap_or_default()) // the data follows at [10]
    }

    // ---- what the mouse thread asks ----------------------------------------------

    /// Is the mouse there (through the receiver, it may be asleep)?
    fn online(&mut self) -> Result<(), Error> {
        if !self.receiver {
            return Ok(());
        }
        match self.family {
            Family::Compx => match self.compx(0x03, 0, 0)?[0] {
                1 => Ok(()),
                _ => Err(Error::Asleep),
            },
            Family::BitMouse => {
                if self.bitmouse(0, 0x81, &[], 2, 1)?.first().copied().unwrap_or(0) == 0 {
                    return Err(Error::Asleep); // no mouse paired with the receiver right now
                }
                match self.bitmouse(1, 0x23, &[], 2, 1)?.first() {
                    Some(0) => Ok(()),
                    _ => Err(Error::Asleep),
                }
            }
        }
    }

    /// (active stage, how many); a garbled answer gives the last good one.
    pub fn stages(&mut self) -> Result<(usize, usize), Error> {
        let (active, count) = match self.family {
            Family::Compx => {
                // each value is followed by 0x55 less it
                let d = self.compx(0x08, 0x00, 10)?;
                if d[2].wrapping_add(d[3]) != 0x55 || d[4].wrapping_add(d[5]) != 0x55 {
                    return Ok(self.last_stages);
                }
                (d[4] as usize, d[2] as usize)
            }
            Family::BitMouse => {
                let d = self.bitmouse_read(1, 2)?;
                (d.first().copied().unwrap_or(0) as usize, d.get(1).copied().unwrap_or(0) as usize)
            }
        };
        if count == 0 || count > 8 {
            return Ok(self.last_stages); // not a believable answer
        }
        self.last_stages = (if active >= count { active.saturating_sub(1).min(count - 1) } else { active }, count);
        Ok(self.last_stages)
    }

    /// The DPI of the active stage (0 when it was never told); answering at all means it is awake. A reading
    /// that makes no sense gives the last good one: the card shows a change only when there is one.
    pub fn dpi(&mut self) -> Result<u16, Error> {
        self.online()?;
        match self.read_dpi() {
            Ok(dpi) if dpi > 0 => self.last_dpi = dpi,
            Ok(_) | Err(Error::Asleep) => {} // garbled or not answered in time: it did answer online
            Err(e) => return Err(e),
        }
        Ok(self.last_dpi)
    }

    fn read_dpi(&mut self) -> Result<u16, Error> {
        let (stage, count) = self.stages()?;
        if count == 0 {
            return Ok(0);
        }
        let dpi = match self.family {
            Family::Compx => {
                let d = self.compx(0x08, 0x0c + 8 * (stage as u16 / 2), 8)?;
                let o = (stage % 2) * 4;
                let (lo, extra) = (d[o] as u32, d[o + 2] as u32);
                let x = ((lo | (extra >> 2 & 3) << 8) + 1) * 50;
                if extra & 1 != 0 {
                    x * 2
                } else {
                    x
                }
            }
            Family::BitMouse => {
                let d = self.bitmouse_read(3 + 8 * stage as u16, 2)?;
                u16::from_le_bytes([d.first().copied().unwrap_or(0), d.get(1).copied().unwrap_or(0)]) as u32
            }
        };
        Ok(if (50..=42000).contains(&dpi) { dpi as u16 } else { 0 })
    }

    /// (percent, on the charger).
    pub fn battery(&mut self) -> Result<(Option<u8>, bool), Error> {
        let (percent, charging) = match self.family {
            Family::Compx => {
                let d = self.compx(0x04, 0, 0)?;
                (d[0], d[1] == 1)
            }
            Family::BitMouse => {
                let target = u8::from(self.receiver);
                let p = self.bitmouse(target, 0x07, &[], 2, 1)?.first().copied().unwrap_or(0xff);
                let c = self.bitmouse(target, 0x0f, &[], 2, 1).ok().and_then(|d| d.first().copied()) == Some(1);
                (p, c)
            }
        };
        if percent > 100 {
            return Ok((None, charging));
        }
        let shown = match (charging, self.plugged) {
            (false, _) => {
                self.plugged = None;
                self.last_percent = Some(percent);
                percent
            }
            (true, None) => {
                // just plugged in: from the percent before (found charging: what it tells, nothing better)
                let from = self.last_percent.unwrap_or(percent);
                self.plugged = Some((Instant::now(), from));
                from
            }
            (true, Some((at, from))) => {
                let most = from as f32 + at.elapsed().as_secs_f32() / 60.0 * PERCENT_PER_MINUTE;
                (percent as f32).min(most).max(from as f32).min(100.0) as u8
            }
        };
        Ok((Some(shown), charging))
    }
}

/// The protocol a collection speaks, if it is one of ours.
fn family(d: &HidInfo) -> Option<Family> {
    if COMPX.contains(&d.pid) && d.usage_page == 0xff02 && d.usage == 2 && d.output_len == 17 {
        Some(Family::Compx)
    } else if BITMOUSE.contains(&d.pid) && d.usage_page == 0xff05 && d.usage == 1 {
        Some(Family::BitMouse)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    /// The checksums of two requests the web driver sends (battery 0x49, EEPROM read 0x3B).
    #[test]
    fn compx_checksum() {
        let sum = |r: &[u8]| 0x55u8.wrapping_sub(r.iter().fold(0u8, |s, &b| s.wrapping_add(b)));
        let mut battery = [0u8; 16];
        battery[0] = 8;
        battery[1] = 4;
        assert_eq!(sum(&battery), 0x49);
        let mut eeprom = [0u8; 16];
        (eeprom[0], eeprom[1], eeprom[5]) = (8, 8, 10);
        assert_eq!(sum(&eeprom), 0x3b);
    }

    /// The BitMouse battery request sums to 0x7C (target 0) / 0x7D (target 1).
    #[test]
    fn bitmouse_checksum() {
        for target in [0u8, 1] {
            let body = [0x72u8, 2, 0, target, 0x07, 1];
            assert_eq!(body.iter().fold(0u8, |s, &b| s.wrapping_add(b)), 0x7c + target);
        }
    }
}
