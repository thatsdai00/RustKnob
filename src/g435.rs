// Logitech G435 headset through its LIGHTSPEED dongle (Holtek bridge, 046D:0ADF): is it on, and its battery.
// Driven by the mouse thread in mouse.rs like the mice; only the talking is here. The protocol comes from
// DsGrilo/G435-Battery-app (MIT), which read it out of Logitech's own G435Update tool:
//
//   out report 0, 64 bytes: [command, data length, crc lo, crc hi, data...], the crc CRC-16/XMODEM over the
//   64 bytes with its own field zero. After each, GetInputReport answers 'O' (ok) at [1].
//     0x85 [1]  the bridge's power saving off (the official tool does it on connecting)
//     0x80      carries a radio command: ['P', 'A', category, transmission, length, ...]
//       category 0x0E (link): E0 get mode (1: remote), E1 set mode, E6 connect status (bit 0x08: headset linked)
//       category 0x06 (OTA):  [0, 0x31] battery, answered 06 (transmission | 0x80) 00 mV lo mV hi
//   in reports: [size] then frames 'P' 'I' type seq timestamp (u32) length (u16 LE) payload
//
// The battery is only told in the dongle's remote mode: it is switched there for the read and back after.
// The percent comes from the cell's voltage (it reads high while charging; the charger is not told). The
// first G435 receiver (046D:0ACB, Realtek bridge) speaks something else and is not supported.

use std::time::{Duration, Instant};

use crate::mouse::{Error, HidIo, HidInfo};

const VID: u16 = 0x046d;
const PID: u16 = 0x0adf;
const ANSWER_WITHIN: Duration = Duration::from_millis(1500);
const LINK_EVERY: Duration = Duration::from_secs(5); // the mouse thread asks every 400 ms: the dongle less often

pub struct G435 {
    io: HidIo,
    pub path: Vec<u16>,
    transmission: u8,
    linked: Option<(Instant, Result<(), Error>)>, // the last look at the headset, and when
}

impl G435 {
    /// The dongle's vendor collection.
    pub fn preferred() -> Option<Vec<u16>> {
        let all = unsafe { crate::mouse::hid_list("vid_046d") };
        let ours = |d: &&HidInfo| d.vid == VID && d.pid == PID && d.usage_page == 0xff00;
        all.iter().find(ours).map(|d| d.path.clone())
    }

    pub fn open(path: Vec<u16>) -> Option<G435> {
        let io = HidIo::open(&path)?;
        let mut g = G435 { io, path, transmission: 0, linked: None };
        let _ = g.command(0x85, &[1], false);
        Some(g)
    }

    /// One report to the bridge, and its 'O'.
    fn command(&mut self, command: u8, data: &[u8], check: bool) -> Result<(), Error> {
        let mut r = [0u8; 65];
        r[1] = command;
        r[2] = data.len() as u8;
        r[5..5 + data.len()].copy_from_slice(data);
        let crc = crc16(&r[1..]);
        r[3..5].copy_from_slice(&crc.to_le_bytes());
        self.io.write(&r)?;
        std::thread::sleep(Duration::from_millis(10));
        let reply = self.io.input_report().ok_or(Error::Io)?;
        if check && reply.get(1) != Some(&b'O') {
            return Err(Error::Asleep);
        }
        Ok(())
    }

    /// A radio command; returns its transmission number (answers carry it).
    fn radio(&mut self, mut cmd: Vec<u8>) -> Result<u8, Error> {
        self.transmission = self.transmission % 127 + 1;
        cmd[3] = self.transmission;
        self.command(0x80, &cmd, true)?;
        Ok(self.transmission)
    }

    /// The first frame payload `take` takes, within ANSWER_WITHIN.
    fn wait<R>(&mut self, mut take: impl FnMut(&[u8]) -> Option<R>) -> Result<R, Error> {
        let until = Instant::now() + ANSWER_WITHIN;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Asleep);
            }
            let Some(r) = self.io.read(left)? else { continue };
            // the report id first, then [size] and the frames
            let data = r.get(2..2 + *r.get(1).unwrap_or(&0) as usize).unwrap_or(&[]);
            let mut i = 0;
            while i + 10 <= data.len() && &data[i..i + 2] == b"PI" {
                let len = u16::from_le_bytes([data[i + 8], data[i + 9]]) as usize;
                let payload = data.get(i + 10..i + 10 + len).unwrap_or(&[]);
                if let Some(v) = take(payload) {
                    return Ok(v);
                }
                i += 10 + len;
            }
        }
    }

    /// A link command's one-byte answer.
    fn link(&mut self, id: u8, params: &[u8]) -> Result<u8, Error> {
        let mut c = vec![0x50, 0x41, 0x0e, 0, 1 + params.len() as u8, id];
        c.extend_from_slice(params);
        self.radio(c)?;
        self.wait(|p| (p.len() >= 2 && p[0] == id).then(|| p[1]))
    }

    fn set_remote(&mut self, on: bool) -> Result<(), Error> {
        self.radio(vec![0x50, 0x41, 0x0e, 0, 2, 0xe1, on as u8])?;
        if self.link(0xe0, &[])? == on as u8 {
            Ok(())
        } else {
            Err(Error::Asleep)
        }
    }

    /// Is the headset on and linked? Looked at every few seconds; in between the last answer stands.
    pub fn awake(&mut self) -> Result<u16, Error> {
        if let Some((at, last)) = &self.linked {
            if at.elapsed() < LINK_EVERY {
                return last.clone().map(|_| 0);
            }
        }
        let now = match self.link(0xe6, &[]) {
            Ok(v) if v & 0x08 != 0 => Ok(()),
            Ok(_) | Err(Error::Asleep) => Err(Error::Asleep),
            Err(Error::Io) => return Err(Error::Io),
        };
        self.linked = Some((Instant::now(), now.clone()));
        now.map(|_| 0)
    }

    fn millivolts(&mut self) -> Result<u16, Error> {
        let t = self.radio(vec![0x50, 0x41, 0x06, 0, 1, 0, 0x31])?;
        self.wait(|p| (p.len() >= 5 && p[0] == 0x06 && p[1] & 0x7f == t && p[2] == 0).then(|| u16::from_le_bytes([p[3], p[4]])))
    }

    /// (percent, on the charger: never told).
    pub fn battery(&mut self) -> Result<(Option<u8>, bool), Error> {
        let remote = self.link(0xe0, &[])? == 1;
        if !remote {
            self.set_remote(true)?;
        }
        let mv = self.millivolts();
        if !remote {
            let _ = self.set_remote(false); // as it was: the dongle's own mode
        }
        let mv = mv?;
        Ok(((2500..=4600).contains(&mv).then(|| percent(mv)), false))
    }
}

fn crc16(data: &[u8]) -> u16 {
    let mut c: u16 = 0;
    for &b in data {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
        }
    }
    c
}

/// A 3.7 V lithium cell's voltage as a percent.
fn percent(mv: u16) -> u8 {
    const CURVE: [(u16, u8); 11] = [(4200, 100), (4100, 90), (4000, 80), (3900, 65), (3800, 50), (3750, 40), (3700, 30), (3650, 20), (3600, 10), (3500, 5), (3300, 0)];
    if mv >= CURVE[0].0 {
        return 100;
    }
    for w in CURVE.windows(2) {
        let ((hi_mv, hi), (lo_mv, lo)) = (w[0], w[1]);
        if mv >= lo_mv {
            return (lo as f32 + (hi - lo) as f32 * (mv - lo_mv) as f32 / (hi_mv - lo_mv) as f32).round() as u8;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc_and_curve() {
        assert_eq!(super::crc16(b"123456789"), 0x31c3);
        assert_eq!(super::percent(4300), 100);
        assert_eq!(super::percent(3850), 58);
        assert_eq!(super::percent(3200), 0);
    }

    /// Reads the real headset. `cargo test g435 -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn read_g435() {
        let mut g = super::G435::open(super::G435::preferred().expect("no dongle")).expect("open");
        println!("awake {:?}", g.awake());
        let t = std::time::Instant::now();
        println!("battery {:?} in {:?}", g.battery(), t.elapsed());
    }
}
