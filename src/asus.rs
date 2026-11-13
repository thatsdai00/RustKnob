// ASUS ROG Gladius III (Wireless, AimPoint): battery, charging, DPI and wake-ups, reads only. Driven by the mouse
// thread in mouse.rs like the other brands; only the talking is here. The protocol is the one ASUS's Armoury
// Crate speaks, as read out by G-Helper (seerge/g-helper) and libratbag:
//
//   interface 0 (mi_00), output / input report 0, 64 bytes after the id: [command lo, command hi, params...];
//   the answer starts with the same bytes. FF AA instead: the mouse is asleep or out of range.
//     12 00     profile:  [11] active profile, [12] active DPI stage (from 1)
//     12 04 00  DPI:      four stages from [5], each u16 LE: dpi = value * 50 + 50
//     12 04 02  DPI with X and Y apart (AimPoint): four stages from [5], each X then Y (u16 LE)
//     12 07     battery:  [5] percent, [10] non-zero while charging
//
// The plain wired Gladius III (0x197B) has no battery and is left out. Every value is checked before it is
// shown: a wrong reading shows nothing rather than nonsense.

use std::time::Duration;

use crate::mouse::{Error, HidIo, HidInfo};

const VID: u16 = 0x0b05;
const WIRED: [u16; 3] = [0x197d, 0x1a70, 0x1b0a]; // on the cable
const RECEIVERS: [u16; 3] = [0x197f, 0x1a72, 0x1b0c];
const XY: [u16; 4] = [0x1a70, 0x1a72, 0x1b0a, 0x1b0c]; // AimPoint: X and Y DPI apart
const STAGES: usize = 4;
const ANSWER_WITHIN: Duration = Duration::from_millis(300);

pub struct Asus {
    io: HidIo,
    pub path: Vec<u16>,
    xy: bool,
    last_dpi: u16, // the last believable reading: a garbled one shows this instead
}

impl Asus {
    /// The collection to talk to: the mouse on its cable before its receiver.
    pub fn preferred() -> Option<Vec<u16>> {
        let all = unsafe { crate::mouse::hid_list("vid_0b05") };
        let ours = |d: &&HidInfo| {
            d.vid == VID
                && (WIRED.contains(&d.pid) || RECEIVERS.contains(&d.pid))
                && d.output_len == 65
                && String::from_utf16_lossy(&d.path).to_ascii_lowercase().contains("mi_00")
        };
        all.iter().filter(ours).min_by_key(|d| !WIRED.contains(&d.pid)).map(|d| d.path.clone())
    }

    pub fn open(path: Vec<u16>) -> Option<Asus> {
        let io = HidIo::open(&path)?;
        let xy = XY.contains(&io.pid);
        Some(Asus { io, path, xy, last_dpi: 0 })
    }

    /// One request; the answer that echoes its command bytes.
    fn ask(&mut self, command: &[u8]) -> Result<Vec<u8>, Error> {
        let mut r = vec![0u8; 65];
        r[1..1 + command.len()].copy_from_slice(command);
        self.io.ask(&r, ANSWER_WITHIN, |a| {
            if a.len() < 13 {
                return None;
            }
            if a[1] == 0xff && a[2] == 0xaa {
                return Some(Err(Error::Asleep));
            }
            (a[1..1 + command.len()] == *command).then(|| Ok(a.to_vec()))
        })
    }

    /// (active stage, how many).
    pub fn stages(&mut self) -> Result<(usize, usize), Error> {
        let a = self.ask(&[0x12, 0x00, 0x00])?;
        Ok(match a[12] as usize {
            s @ 1..=STAGES => (s - 1, STAGES),
            _ => (0, 0), // not a believable answer: no markers
        })
    }

    /// The DPI of the active stage (0 when it was never told); answering at all means it is awake.
    pub fn dpi(&mut self) -> Result<u16, Error> {
        let (stage, count) = self.stages()?;
        if count == 0 {
            return Ok(self.last_dpi);
        }
        let a = self.ask(&[0x12, 0x04, if self.xy { 0x02 } else { 0x00 }])?;
        let at = 5 + stage * if self.xy { 4 } else { 2 };
        let dpi = u16::from_le_bytes([a[at], a[at + 1]]) as u32 * 50 + 50;
        if (100..=36000).contains(&dpi) {
            self.last_dpi = dpi as u16;
        }
        Ok(self.last_dpi)
    }

    /// (percent, on the charger).
    pub fn battery(&mut self) -> Result<(Option<u8>, bool), Error> {
        let a = self.ask(&[0x12, 0x07])?;
        Ok(((a[5] <= 100).then_some(a[5]), a[10] != 0))
    }
}
