// The device protocols are not in the exe: each family is a JSON file (devices/*.json in the repository,
// fetched for the devices that are plugged in, see catalog.rs) and this runs it. A protocol is a list of steps
// over one HID collection: send a frame (output, SetOutputReport or feature report, with a checksum filled in),
// read the answer (an input report that meets a condition, or a feature / input report asked for), check it,
// keep values in variables, try alternatives, retry, loop over receiver slots, and emit what it read.
// The language is in devices/README.md.
//
// A step that fails ends the block it is in: `first` then tries its next branch, `retry` its next round,
// `each` its next item; at the top the run ends with what was emitted so far.

use crate::json::Value;

use super::Kind;

/// What a protocol talks through: a HID collection (hid.rs), or a recording in the tests.
pub trait Port {
    /// The collection's report sizes, report id included: frames are padded to these.
    fn lens(&self) -> (usize, usize); // (output, feature)
    fn out(&mut self, frame: &[u8], set_output: bool) -> bool;
    fn fset(&mut self, frame: &[u8]) -> bool;
    fn fget(&mut self, id: u8) -> Option<Vec<u8>>;
    fn iget(&mut self, id: u8) -> Option<Vec<u8>>;
    /// The next input report within `ms` (as it came, report id first).
    fn read(&mut self, ms: u32) -> Option<Vec<u8>>;
    fn flush(&mut self);
    fn sleep(&mut self, ms: u32);
}

/// One device a run read.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Emit {
    pub slot: String,
    pub id: Option<String>, // replaces vid:pid:slot in the key (a unit id: one key over cable and receiver)
    pub kind: Option<Kind>,
    pub name: Option<String>,
    pub percent: Option<u8>,
    pub charging: bool,
    pub full: bool,
    pub state: u8, // 0 awake, 1 asleep, 2 offline
}

struct Run<'a> {
    port: &'a mut dyn Port,
    file: &'a Value,
    vars: Vec<(String, i64)>,
    r: Vec<u8>,
    out: Vec<Emit>,
    budget: u32, // steps left: a broken file must not spin the thread
}

#[derive(PartialEq)]
enum Flow {
    Next,
    Fail,
    Stop,
}

/// Runs `steps` of a protocol from `file` with the device's variables; returns what it emitted.
pub fn run(port: &mut dyn Port, file: &Value, steps: &Value, vars: Vec<(String, i64)>) -> Vec<Emit> {
    let mut run = Run { port, file, vars, r: Vec::new(), out: Vec::new(), budget: 4000 };
    run.block(steps);
    run.out
}

pub fn kind(name: &str) -> Option<Kind> {
    Some(match name {
        "mouse" => Kind::Mouse,
        "keyboard" => Kind::Keyboard,
        "headset" => Kind::Headset,
        "gamepad" => Kind::Gamepad,
        "phone" => Kind::Phone,
        "other" => Kind::Other,
        _ => return None,
    })
}

impl Run<'_> {
    fn block(&mut self, steps: &Value) -> Flow {
        for step in steps.arr() {
            match self.step(step) {
                Flow::Next => {}
                f => return f,
            }
        }
        Flow::Next
    }

    fn ok(b: bool) -> Flow {
        if b {
            Flow::Next
        } else {
            Flow::Fail
        }
    }

    fn step(&mut self, s: &Value) -> Flow {
        if self.budget == 0 {
            return Flow::Fail;
        }
        self.budget -= 1;
        let Value::Obj(fields) = s else { return Flow::Fail };
        let Some((op, arg)) = fields.first() else { return Flow::Fail };
        match op.as_str() {
            "out" | "oset" => {
                let Some(frame) = self.frame(arg, s.get("ck"), self.port.lens().0) else { return Flow::Fail };
                Self::ok(self.port.out(&frame, op == "oset"))
            }
            "fset" => {
                let Some(frame) = self.frame(arg, s.get("ck"), self.port.lens().1) else { return Flow::Fail };
                Self::ok(self.port.fset(&frame))
            }
            "fget" | "iget" => {
                let id = self.num(arg).unwrap_or(0) as u8;
                let got = if op == "fget" { self.port.fget(id) } else { self.port.iget(id) };
                match got {
                    Some(r) => {
                        self.r = r;
                        Flow::Next
                    }
                    None => Flow::Fail,
                }
            }
            "read" => {
                // input reports until one meets "if" (any, without it), within the time given
                let ms = self.num(arg).unwrap_or(500).clamp(1, 5000) as u32;
                let until = std::time::Instant::now() + std::time::Duration::from_millis(ms as u64);
                loop {
                    let left = until.saturating_duration_since(std::time::Instant::now()).as_millis() as u32;
                    if left == 0 {
                        return Flow::Fail;
                    }
                    let Some(r) = self.port.read(left) else { return Flow::Fail };
                    self.r = crate::hid::payload(&r).to_vec();
                    if s.get("if").is_none_or(|c| self.cond(c)) {
                        return Flow::Next;
                    }
                }
            }
            "flush" => {
                self.port.flush();
                Flow::Next
            }
            "sleep" => {
                self.port.sleep(self.num(arg).unwrap_or(0).clamp(0, 2000) as u32);
                Flow::Next
            }
            "need" => Self::ok(self.cond(arg)),
            "set" => {
                if let Value::Obj(vs) = arg {
                    for (k, v) in vs {
                        let Some(n) = self.num(v) else { return Flow::Fail };
                        self.var_set(k, n);
                    }
                }
                Flow::Next
            }
            "first" => {
                for branch in arg.arr() {
                    match self.block(branch) {
                        Flow::Fail => continue,
                        f => return f,
                    }
                }
                Flow::Fail
            }
            "retry" => {
                let gap = s.get("gap").and_then(|g| self.num(g)).unwrap_or(0).clamp(0, 2000) as u32;
                let body = s.get("do").unwrap_or(&Value::Null);
                for i in 0..self.num(arg).unwrap_or(1).clamp(1, 20) {
                    if i > 0 && gap > 0 {
                        self.port.sleep(gap);
                    }
                    match self.block(body) {
                        Flow::Fail => continue,
                        f => return f,
                    }
                }
                Flow::Fail
            }
            "each" => {
                let name = arg.str().unwrap_or("i").to_string();
                let body = s.get("do").unwrap_or(&Value::Null);
                let items: Vec<i64> = s.get("in").map(|v| v.arr().iter().filter_map(|x| self.num(x)).collect()).unwrap_or_default();
                for item in items {
                    self.var_set(&name, item);
                    if self.block(body) == Flow::Stop {
                        return Flow::Stop;
                    }
                }
                Flow::Next
            }
            // like "each", but done at the first item whose block went through (the variable keeps that item)
            "any" => {
                let name = arg.str().unwrap_or("i").to_string();
                let body = s.get("do").unwrap_or(&Value::Null);
                let items: Vec<i64> = s.get("in").map(|v| v.arr().iter().filter_map(|x| self.num(x)).collect()).unwrap_or_default();
                for item in items {
                    self.var_set(&name, item);
                    match self.block(body) {
                        Flow::Fail => continue,
                        f => return f,
                    }
                }
                Flow::Fail
            }
            // a block the file names in "subs"
            "call" => {
                let file = self.file;
                match arg.str().and_then(|n| file.get("subs")?.get(n)) {
                    Some(body) => match self.block(body) {
                        Flow::Stop => Flow::Next,
                        f => f,
                    },
                    None => Flow::Fail,
                }
            }
            "emit" => {
                self.emit(arg);
                Flow::Next
            }
            "stop" => Flow::Stop,
            _ => Flow::Fail, // "fail", or a step this build does not know
        }
    }

    fn emit(&mut self, e: &Value) {
        let state = match e.get("state").and_then(Value::str) {
            Some("asleep") => 1,
            Some("offline") => 2,
            _ if e.get("offline").is_some_and(|c| self.cond(c)) => 2,
            _ if e.get("asleep").is_some_and(|c| self.cond(c)) => 1,
            _ => 0,
        };
        let text = |v: Option<&Value>, me: &Self| v.and_then(|v| v.str().map(str::to_string).or_else(|| me.num(v).map(|n| n.to_string())));
        let percent = e.get("percent").and_then(|p| self.num(p)).filter(|p| (0..=100).contains(p)).map(|p| p as u8);
        let emit = Emit {
            slot: text(e.get("slot"), self).unwrap_or_default(),
            id: e.get("id").and_then(|v| self.text(v)),
            kind: e.get("kind").and_then(Value::str).and_then(kind),
            name: e.get("name").and_then(Value::str).map(str::to_string),
            percent,
            charging: e.get("charging").is_some_and(|c| self.cond(c)),
            full: e.get("full").is_some_and(|c| self.cond(c)),
            state,
        };
        self.out.push(emit);
    }

    // ---- frames ------------------------------------------------------------------

    /// The bytes of a frame ("08 03 $idx 00"), padded to `len` (at least as long as given), checksum filled in.
    fn frame(&self, hex: &Value, ck: Option<&Value>, len: usize) -> Option<Vec<u8>> {
        let mut f = Vec::new();
        for tok in hex.str()?.split_ascii_whitespace() {
            f.push(match tok.strip_prefix('$') {
                Some(v) => self.var(v)? as u8,
                None => u8::from_str_radix(tok, 16).ok()?,
            });
        }
        if f.len() < len {
            f.resize(len, 0);
        }
        if let Some(ck) = ck {
            let (at, v) = self.checksum(ck, &f)?;
            *f.get_mut(at)? = v as u8;
            if ck.get("t").and_then(Value::str) == Some("sum16le") {
                *f.get_mut(at + 1)? = (v >> 8) as u8;
            }
        }
        Some(f)
    }

    /// A checksum over `f`: {"t": "xor" | "sum" | "sum16le", "from": a, "to": b (inclusive), "at": c, "sub": k}
    /// ("sub": the result is k - sum). Returns (where it goes, value).
    fn checksum(&self, ck: &Value, f: &[u8]) -> Option<(usize, u32)> {
        let n = |k: &str| ck.get(k).and_then(|v| self.num(v));
        let (from, to, at) = (n("from")? as usize, n("to")? as usize, n("at")? as usize);
        let bytes = f.get(from..=to)?;
        let v = match ck.get("t").and_then(Value::str)? {
            "xor" => bytes.iter().fold(0u32, |a, &b| a ^ b as u32),
            "sum16le" => bytes.iter().map(|&b| b as u32).sum::<u32>() & 0xffff,
            _ => {
                let sum = bytes.iter().map(|&b| b as u32).sum::<u32>();
                match n("sub") {
                    Some(k) => (k as u32).wrapping_sub(sum) & 0xff,
                    None => sum & 0xff,
                }
            }
        };
        Some((at, v))
    }

    // ---- values ------------------------------------------------------------------

    fn var(&self, name: &str) -> Option<i64> {
        self.vars.iter().rev().find(|(k, _)| k == name).map(|(_, v)| *v)
    }

    fn var_set(&mut self, name: &str, v: i64) {
        match self.vars.iter_mut().find(|(k, _)| k == name) {
            Some(slot) => slot.1 = v,
            None => self.vars.push((name.to_string(), v)),
        }
    }

    fn byte(&self, at: i64) -> Option<i64> {
        self.r.get(usize::try_from(at).ok()?).map(|&b| b as i64)
    }

    /// A number: a literal, "$var", "0x1F", or an expression over the last reply (`r`).
    fn num(&self, v: &Value) -> Option<i64> {
        match v {
            Value::Num(n) => Some(*n as i64),
            Value::Bool(b) => Some(*b as i64),
            Value::Str(s) => match s.strip_prefix('$') {
                Some(name) => self.var(name),
                None => i64::from_str_radix(s.trim_start_matches("0x"), 16).ok(),
            },
            Value::Obj(f) => {
                let (op, a) = f.first()?;
                let args = a.arr();
                let at = |i: usize| args.get(i).and_then(|x| self.num(x));
                match op.as_str() {
                    "b" => self.byte(self.num(a)?),
                    "u16be" | "u16le" => {
                        let o = self.num(a)?;
                        let (x, y) = (self.byte(o)?, self.byte(o + 1)?);
                        Some(if op == "u16be" { x << 8 | y } else { y << 8 | x })
                    }
                    "len" => Some(self.r.len() as i64),
                    "add" => Some(at(0)? + at(1)?),
                    "sub" => Some(at(0)? - at(1)?),
                    "mul" => Some(at(0)? * at(1)?),
                    "div" => at(0)?.checked_div(at(1)?),
                    "and" => Some(at(0)? & at(1)?),
                    "or" => Some(at(0)? | at(1)?),
                    "shr" => Some(at(0)? >> at(1)?.clamp(0, 63)),
                    "shl" => Some(at(0)? << at(1)?.clamp(0, 63)),
                    "min" => Some(at(0)?.min(at(1)?)),
                    "max" => Some(at(0)?.max(at(1)?)),
                    // (x - lo) * 100 / (hi - lo), within 0..100; hi <= lo: x itself, within 0..100
                    "scale" => {
                        let (x, lo, hi) = (at(0)?, at(1)?, at(2)?);
                        Some(if hi > lo { (x - lo) * 100 / (hi - lo) } else { x }.clamp(0, 100))
                    }
                    // a table by index: ["pick", [x, [t0, t1, ...]]]
                    "pick" => {
                        let i = usize::try_from(at(0)?).ok()?;
                        self.num(args.get(1)?.arr().get(i)?)
                    }
                    // a falling (mV, %) curve of the file, by name
                    "curve" => {
                        let points: Vec<(u16, u8)> = self.file.get("curves")?.get(args.first()?.str()?)?.arr().iter().filter_map(|p| Some((self.num(p.arr().first()?)? as u16, self.num(p.arr().get(1)?)? as u8))).collect();
                        let mv = at(1)?.clamp(0, 65535) as u16;
                        (!points.is_empty()).then(|| super::on_curve(&points, mv) as i64)
                    }
                    // the first of the values that is known
                    "or_else" => args.iter().find_map(|x| self.num(x)),
                    "if" => {
                        if self.cond(args.first()?) {
                            at(1)
                        } else {
                            at(2)
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Text for a key: {"hex": [from, n]} the reply's bytes as hex digits, or a number.
    fn text(&self, v: &Value) -> Option<String> {
        if let Some(h) = v.get("hex") {
            let (from, n) = (self.num(h.arr().first()?)?, self.num(h.arr().get(1)?)?);
            let bytes = self.r.get(usize::try_from(from).ok()?..usize::try_from(from + n).ok()?)?;
            return Some(bytes.iter().map(|b| format!("{b:02X}")).collect());
        }
        v.str().map(str::to_string).or_else(|| self.num(v).map(|n| n.to_string()))
    }

    fn cond(&self, c: &Value) -> bool {
        match c {
            Value::Bool(b) => *b,
            Value::Obj(f) => {
                let Some((op, a)) = f.first() else { return false };
                let args = a.arr();
                let at = |i: usize| args.get(i).and_then(|x| self.num(x));
                let cmp = |f: fn(i64, i64) -> bool| matches!((at(0), at(1)), (Some(x), Some(y)) if f(x, y));
                match op.as_str() {
                    "eq" => cmp(|x, y| x == y),
                    "ne" => cmp(|x, y| x != y),
                    "lt" => cmp(|x, y| x < y),
                    "le" => cmp(|x, y| x <= y),
                    "gt" => cmp(|x, y| x > y),
                    "ge" => cmp(|x, y| x >= y),
                    "in" => at(0).is_some_and(|x| args.get(1).is_some_and(|l| l.arr().iter().any(|v| self.num(v) == Some(x)))),
                    "known" => self.num(a).is_some(),
                    "len" => self.num(a).is_some_and(|n| self.r.len() as i64 >= n),
                    "all" => args.iter().all(|x| self.cond(x)),
                    "any" => args.iter().any(|x| self.cond(x)),
                    "not" => !self.cond(a),
                    // the reply's own checksum is right
                    "ck" => self.checksum(a, &self.r).is_some_and(|(at, v)| {
                        let low = self.r.get(at).copied();
                        if a.get("t").and_then(Value::str) == Some("sum16le") {
                            low == Some(v as u8) && self.r.get(at + 1).copied() == Some((v >> 8) as u8)
                        } else {
                            low == Some(v as u8)
                        }
                    }),
                    // the bytes from an offset are these: ["at", [off, "08 03"]]
                    "at" => (|| {
                        let off = usize::try_from(at(0)?).ok()?;
                        let want = args.get(1)?.str()?;
                        let mut i = off;
                        for tok in want.split_ascii_whitespace() {
                            let b = match tok.strip_prefix('$') {
                                Some(v) => self.var(v)? as u8,
                                None => u8::from_str_radix(tok, 16).ok()?,
                            };
                            if self.r.get(i) != Some(&b) {
                                return Some(false);
                            }
                            i += 1;
                        }
                        Some(true)
                    })()
                    .unwrap_or(false),
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

// ---- the tests in each JSON file -------------------------------------------------

/// Plays back what a device would answer: each "fget"/"iget"/"read" takes the next recorded reply.
#[cfg(test)]
pub struct Playback {
    pub replies: std::collections::VecDeque<Vec<u8>>,
    pub sent: Vec<Vec<u8>>,
    pub lens: (usize, usize),
}

#[cfg(test)]
impl Port for Playback {
    fn lens(&self) -> (usize, usize) {
        self.lens
    }
    fn out(&mut self, frame: &[u8], _: bool) -> bool {
        self.sent.push(frame.to_vec());
        true
    }
    fn fset(&mut self, frame: &[u8]) -> bool {
        self.sent.push(frame.to_vec());
        true
    }
    fn fget(&mut self, _: u8) -> Option<Vec<u8>> {
        self.replies.pop_front()
    }
    fn iget(&mut self, _: u8) -> Option<Vec<u8>> {
        self.replies.pop_front()
    }
    fn read(&mut self, _: u32) -> Option<Vec<u8>> {
        self.replies.pop_front()
    }
    fn flush(&mut self) {}
    fn sleep(&mut self, _: u32) {}
}
