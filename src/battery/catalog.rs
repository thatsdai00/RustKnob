// Which protocol each product speaks, and the protocols themselves: JSON files in the repository's devices
// folder (devices/index.json maps "VID:PID" or "VID:*" to a family file). Only the files of devices that are
// plugged in are fetched, once, and kept in %LOCALAPPDATA%\RustKnob\devices; the index is looked at again at
// most once a day, and only when a device the copy does not know is plugged in (or once a week). Offline, the
// copies on disk are used. In a development build next to the repository the repository's own files are read.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::hid::{HidInfo, HidIo};
use crate::json::Value;

use super::script::{self, Emit, Port};
use super::{Kind, Reading, Scan, Source, State, ASLEEP_EVERY, AWAKE_EVERY};

const DAY: Duration = Duration::from_secs(24 * 3600);

pub fn driver(scan: &mut Scan, sources: &mut Vec<Box<dyn Source>>) {
    let mut present: Vec<(u16, u16)> = Vec::new();
    for c in scan.free() {
        if !present.contains(&(c.vid, c.pid)) {
            present.push((c.vid, c.pid));
        }
    }
    if present.is_empty() {
        return;
    }
    let store = Store::new();
    let Some(index) = store.index(&present) else { return };
    let ids = index.get("ids");
    let family_of = |vid: u16, pid: u16| ids.and_then(|m| m.get(&format!("{vid:04X}:{pid:04X}")).or_else(|| m.get(&format!("{vid:04X}:*")))).and_then(Value::str);
    // the families needed, in the index's claiming order
    let order: Vec<&str> = index.get("order").map(|o| o.arr().iter().filter_map(Value::str).collect()).unwrap_or_default();
    let mut needed: Vec<&str> = present.iter().filter_map(|&(v, p)| family_of(v, p)).collect();
    needed.sort_by_key(|f| order.iter().position(|o| o == f).unwrap_or(usize::MAX));
    needed.dedup();
    for name in needed {
        let version = index.get("families").and_then(|f| f.get(name)).and_then(Value::num).unwrap_or(0.0) as i64;
        if let Some(file) = store.family(name, version) {
            family(scan, sources, name, &Arc::new(file));
        }
    }
}

/// Opens a source for every present device of one family file.
fn family(scan: &mut Scan, sources: &mut Vec<Box<dyn Source>>, family: &str, file: &Arc<Value>) {
    let except: Vec<(u16, u16)> = file.get("except").map(|e| e.arr().iter().filter_map(|v| id(v.str()?)).collect()).unwrap_or_default();
    for entry in file.get("devices").map(Value::arr).unwrap_or_default() {
        // [ "VID:PID" or "VID:*", protocol, name, kind, {vars} ]
        let e = entry.arr();
        let Some(spec) = e.first().and_then(Value::str) else { continue };
        let Some(proto_name) = e.get(1).and_then(Value::str) else { continue };
        let Some(proto) = file.get("protocols").and_then(|p| p.get(proto_name)) else { continue };
        let (vid, pid) = match spec.split_once(':') {
            Some((v, "*")) => (u16::from_str_radix(v, 16).ok(), None),
            _ => id(spec).map_or((None, None), |(v, p)| (Some(v), Some(p))),
        };
        let Some(vid) = vid else { continue };
        let products: Vec<u16> = match pid {
            Some(p) => vec![p],
            None => scan.products(vid),
        };
        for pid in products {
            if scan.is_claimed(vid, pid) || except.contains(&(vid, pid)) {
                continue;
            }
            let mut vars: Vec<(String, i64)> = vec![("vid".into(), vid as i64), ("pid".into(), pid as i64)];
            if let Some(Value::Obj(v)) = e.get(4) {
                vars.extend(v.iter().filter_map(|(k, x)| Some((k.clone(), x.num()? as i64))));
            }
            let device = Device {
                family: family.to_string(),
                vid,
                pid,
                names: e.get(2).map(|n| match n {
                    Value::Str(s) if !s.is_empty() => vec![(String::new(), s.clone())],
                    Value::Obj(by_slot) => by_slot.iter().filter_map(|(k, v)| Some((k.clone(), v.str()?.to_string()))).collect(),
                    _ => Vec::new(),
                }).unwrap_or_default(),
                kind: e.get(3).and_then(Value::str).and_then(script::kind).or_else(|| proto.get("kind").and_then(Value::str).and_then(script::kind)),
                file: file.clone(),
                proto: proto_name.to_string(),
                vars,
            };
            if let Some(source) = open(scan, device) {
                scan.claim(vid, pid);
                sources.push(Box::new(source));
            }
        }
    }
}

fn id(s: &str) -> Option<(u16, u16)> {
    let (v, p) = s.split_once(':')?;
    Some((u16::from_str_radix(v, 16).ok()?, u16::from_str_radix(p, 16).ok()?))
}

/// One product a family file speaks to.
struct Device {
    family: String,
    vid: u16,
    pid: u16,
    names: Vec<(String, String)>, // by slot ("" for all)
    kind: Option<Kind>,
    file: Arc<Value>,
    proto: String,
    vars: Vec<(String, i64)>,
}

impl Device {
    fn proto(&self) -> &Value {
        self.file.get("protocols").and_then(|p| p.get(&self.proto)).unwrap_or(&Value::Null)
    }

    fn secs(&self, key: &str, default: Duration) -> Duration {
        self.proto().get(key).and_then(Value::num).map_or(default, |s| Duration::from_secs(s as u64))
    }
}

/// The collections the protocol's "pick" takes, in its order: each selector is an object of
/// {"mi", "page", "usage", "out", "in", "feat"} (a number, or [min, max] for a length).
fn candidates<'a>(scan: &'a Scan, d: &Device) -> Vec<&'a HidInfo> {
    let mine: Vec<&HidInfo> = scan.cols.iter().filter(|c| c.is(d.vid, d.pid)).collect();
    let Some(pick) = d.proto().get("pick") else { return mine };
    let fits = |sel: &Value, c: &HidInfo| {
        let n = |k: &str| sel.get(k).and_then(Value::num).map(|x| x as i64);
        let s = |k: &str| sel.get(k).and_then(Value::str).and_then(|h| i64::from_str_radix(h, 16).ok());
        let len = |k: &str, have: u16| match sel.get(k) {
            Some(Value::Num(x)) => have as i64 == *x as i64,
            Some(Value::Arr(r)) => r.first().and_then(Value::num).is_none_or(|lo| have as f64 >= lo) && r.get(1).and_then(Value::num).is_none_or(|hi| have as f64 <= hi),
            _ => true,
        };
        n("mi").is_none_or(|m| c.interface == Some(m as u8))
            && s("page").is_none_or(|p| c.usage_page as i64 == p)
            && s("usage").is_none_or(|u| c.usage as i64 == u)
            && len("out", c.output_len)
            && len("in", c.input_len)
            && len("feat", c.feature_len)
    };
    let mut out = Vec::new();
    for sel in pick.arr() {
        for c in &mine {
            if fits(sel, c) && !out.iter().any(|o: &&HidInfo| o.path == c.path) {
                out.push(*c);
            }
        }
    }
    out
}

/// The first collection that answers; none answering, the first one that opens (its device may be asleep now).
fn open(scan: &Scan, d: Device) -> Option<Scripted> {
    let mode = d.proto().get("open").and_then(Value::str).unwrap_or("rw");
    let mut fallback: Option<(HidIo, HidInfo)> = None;
    for c in candidates(scan, &d) {
        let io = match mode {
            "feature" => HidIo::open_feature(c),
            "read" => HidIo::open_read(c),
            _ => HidIo::open(&c.path),
        };
        let Some(mut io) = io else { continue };
        let emits = script::run(&mut io, &d.file, d.proto().get("run").unwrap_or(&Value::Null), d.vars.clone());
        if !emits.is_empty() {
            let product = crate::hid::product(&c.path);
            let kind = kind_of(scan, &d);
            let mut s = Scripted { device: d, io, product, kind, first: Some(emits), keys: Vec::new() };
            s.product = s.product.take().map(|p| strip(&p, s.device.proto()));
            return Some(s);
        }
        if fallback.is_none() {
            fallback = Some((io, c.clone()));
        }
    }
    let (io, c) = fallback?;
    let product = crate::hid::product(&c.path).map(|p| strip(&p, d.proto()));
    let kind = kind_of(scan, &d);
    Some(Scripted { device: d, io, product, kind, first: None, keys: Vec::new() })
}

/// What the product's own collections say it is (a mouse collection: a mouse).
fn kind_of(scan: &Scan, d: &Device) -> Option<Kind> {
    Some(Kind::from_collections(&scan.cols, d.vid, d.pid)).filter(|k| *k != Kind::Other)
}

/// The product string without the receiver words a protocol lists in "strip".
fn strip(name: &str, proto: &Value) -> String {
    let mut n = name.trim().to_string();
    for s in proto.get("strip").map(Value::arr).unwrap_or_default().iter().filter_map(Value::str) {
        if let Some(rest) = n.strip_suffix(s) {
            n = rest.trim_end().to_string();
        }
    }
    n
}

struct Scripted {
    device: Device,
    io: HidIo,
    product: Option<String>,
    kind: Option<Kind>, // from its collections, when neither the file nor the run says
    first: Option<Vec<Emit>>, // the run that found it, used as the first poll
    keys: Vec<(String, String, Kind)>, // what it reported before: kept (asleep) when a run gives nothing
}

impl Source for Scripted {
    fn poll(&mut self, out: &mut Vec<Reading>) -> Duration {
        let d = &self.device;
        let emits = self.first.take().unwrap_or_else(|| script::run(&mut self.io, &d.file, d.proto().get("run").unwrap_or(&Value::Null), d.vars.clone()));
        if emits.is_empty() {
            out.extend(self.keys.iter().map(|(k, n, kind)| Reading::new(k, n, *kind).asleep()));
            return d.secs("asleep", ASLEEP_EVERY);
        }
        let grace = d.secs("grace", super::GRACE);
        let mut awake = false;
        for e in emits {
            let key = match &e.id {
                Some(id) => format!("{}:{id}", d.family),
                None if e.slot.is_empty() => format!("{}:{:04X}:{:04X}", d.family, d.vid, d.pid),
                None => format!("{}:{:04X}:{:04X}:{}", d.family, d.vid, d.pid, e.slot),
            };
            let named = |slot: &str| d.names.iter().find(|(s, _)| s == slot).map(|(_, n)| n.clone());
            let name = e.name.clone().or_else(|| named(&e.slot)).or_else(|| named("")).or_else(|| self.product.clone().filter(|p| !p.is_empty())).unwrap_or_else(|| "Wireless device".into());
            let kind = e.kind.or(d.kind).or(self.kind).unwrap_or_else(|| Kind::from_name(&name));
            let mut r = Reading::new(&key, &name, kind);
            (r.percent, r.charging, r.full, r.grace) = (e.percent, e.charging, e.full, grace);
            r.state = [State::Awake, State::Asleep, State::Offline][e.state.min(2) as usize];
            awake |= r.state == State::Awake;
            if !self.keys.iter().any(|k| k.0 == key) {
                self.keys.push((key, name, kind));
            }
            out.push(r);
        }
        if awake {
            d.secs("every", AWAKE_EVERY)
        } else {
            d.secs("asleep", ASLEEP_EVERY)
        }
    }
}

impl Port for HidIo {
    fn lens(&self) -> (usize, usize) {
        HidIo::lens(self)
    }
    fn out(&mut self, frame: &[u8], set_output: bool) -> bool {
        if set_output {
            self.set_output(frame)
        } else {
            self.write(frame).is_ok()
        }
    }
    fn fset(&mut self, frame: &[u8]) -> bool {
        self.set_feature(frame)
    }
    fn fget(&mut self, id: u8) -> Option<Vec<u8>> {
        self.get_feature(id)
    }
    fn iget(&mut self, id: u8) -> Option<Vec<u8>> {
        self.get_input(id)
    }
    fn read(&mut self, ms: u32) -> Option<Vec<u8>> {
        HidIo::read(self, Duration::from_millis(ms as u64)).ok().flatten()
    }
    fn flush(&mut self) {
        HidIo::flush(self)
    }
    fn sleep(&mut self, ms: u32) {
        std::thread::sleep(Duration::from_millis(ms as u64));
    }
}

// ---- the files -------------------------------------------------------------------

/// Where the files are: the repository (development build next to it), else the local copy fed from GitHub.
struct Store {
    dir: PathBuf,
    online: bool,
}

impl Store {
    fn new() -> Store {
        let exe = crate::install::exe_dir();
        // targetelease (or its deps, for the tests) inside the repository
        if let Some(repo) = exe.ancestors().skip(2).take(2).map(|p| p.join("devices")).find(|d| d.join("index.json").exists()) {
            return Store { dir: repo, online: false };
        }
        Store { dir: crate::install::env_path("LOCALAPPDATA").join("RustKnob").join("devices"), online: true }
    }

    fn read(&self, name: &str) -> Option<(Value, Duration)> {
        let path = self.dir.join(format!("{name}.json"));
        let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok()).unwrap_or_default();
        Some((Value::parse(&std::fs::read_to_string(path).ok()?)?, age))
    }

    /// Fetched from the repository and kept (written whole, then renamed).
    fn fetch(&self, name: &str) -> Option<Value> {
        if !self.online {
            return None;
        }
        let url = format!("https://raw.githubusercontent.com/{}/main/devices/{name}.json", crate::update::REPO);
        let bytes = crate::update::get(&url, 1 << 20)?;
        let value = Value::parse(std::str::from_utf8(&bytes).ok()?)?;
        let (part, file) = (self.dir.join(format!("{name}.part")), self.dir.join(format!("{name}.json")));
        if std::fs::create_dir_all(&self.dir).is_ok() && std::fs::write(&part, &bytes).is_ok() && std::fs::rename(&part, &file).is_err() {
            let _ = std::fs::remove_file(&part);
        }
        Some(value)
    }

    /// The index: the copy, unless it is a week old or a day old and does not know a device that is plugged in.
    fn index(&self, present: &[(u16, u16)]) -> Option<Value> {
        let copy = self.read("index");
        let stale = match &copy {
            None => true,
            Some((v, age)) => {
                let knows = |&(vid, pid): &(u16, u16)| v.get("ids").is_some_and(|m| m.get(&format!("{vid:04X}:{pid:04X}")).or_else(|| m.get(&format!("{vid:04X}:*"))).is_some());
                *age > 7 * DAY || (*age > DAY && !present.iter().all(knows))
            }
        };
        if stale {
            if let Some(v) = self.fetch("index") {
                return Some(v);
            }
            if let Some((_, age)) = &copy {
                // offline: the copy will do; touch it so the next device change does not try again at once
                let _ = age;
                if let Ok(f) = std::fs::File::options().append(true).open(self.dir.join("index.json")) {
                    let _ = f.set_modified(SystemTime::now() - DAY + Duration::from_secs(3600));
                }
            }
        }
        copy.map(|c| c.0)
    }

    /// A family file at least as new as the index says.
    fn family(&self, name: &str, version: i64) -> Option<Value> {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return None; // only plain names: the index never points outside the folder
        }
        let copy = self.read(name).map(|c| c.0);
        if copy.as_ref().is_some_and(|c| c.get("v").and_then(Value::num).unwrap_or(0.0) as i64 >= version) {
            return copy;
        }
        self.fetch(name).or(copy)
    }
}

#[cfg(test)]
mod tests {
    use super::script::{run, Playback};
    use crate::json::Value;

    fn hex(s: &str) -> Vec<u8> {
        s.split_ascii_whitespace().filter_map(|t| u8::from_str_radix(t, 16).ok()).collect()
    }

    /// Every file in devices/ parses, every product in the index has its family, and each file's "tests" give
    /// what they expect: `cargo test --release devices`
    #[test]
    fn devices() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("devices");
        let index = Value::parse(&std::fs::read_to_string(dir.join("index.json")).expect("index.json")).expect("index.json parses");
        let Some(Value::Obj(ids)) = index.get("ids") else { panic!("index: ids") };
        for (id, fam) in ids {
            let fam = fam.str().expect("family name");
            let path = dir.join(format!("{fam}.json"));
            let file = Value::parse(&std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{fam}.json for {id}"))).unwrap_or_else(|| panic!("{fam}.json parses"));
            assert!(index.get("families").and_then(|f| f.get(fam)).and_then(Value::num).is_some(), "{fam}: version in index");
            assert!(index.get("order").is_some_and(|o| o.arr().iter().any(|x| x.str() == Some(fam))), "{fam}: in the index's order");
            let _ = file;
        }
        let mut ran = 0;
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") || path.file_stem().is_some_and(|s| s == "index") {
                continue;
            }
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            let file = Value::parse(&std::fs::read_to_string(&path).unwrap()).unwrap_or_else(|| panic!("{name}.json parses"));
            for d in file.get("devices").map(Value::arr).unwrap_or_default() {
                let p = d.arr().get(1).and_then(Value::str).unwrap_or_else(|| panic!("{name}: device without protocol"));
                assert!(file.get("protocols").and_then(|x| x.get(p)).is_some(), "{name}: protocol {p} missing");
                let id = d.arr()[0].str().unwrap();
                assert!(ids.iter().any(|(k, f)| (k == id || k == &format!("{}:*", &id[..4])) && f.str() == Some(&name)), "{name}: {id} not in the index");
            }
            for (i, t) in file.get("tests").map(Value::arr).unwrap_or_default().iter().enumerate() {
                let proto = file.get("protocols").and_then(|p| p.get(t.get("protocol").and_then(Value::str).unwrap())).unwrap();
                let lens = (t.get("out").and_then(Value::num).unwrap_or(65.0) as usize, t.get("feat").and_then(Value::num).unwrap_or(65.0) as usize);
                let mut port = Playback { replies: t.get("replies").map(Value::arr).unwrap_or_default().iter().map(|r| hex(r.str().unwrap())).collect(), sent: Vec::new(), lens };
                let mut vars: Vec<(String, i64)> = Vec::new();
                if let Some(Value::Obj(v)) = t.get("vars") {
                    vars.extend(v.iter().map(|(k, x)| (k.clone(), x.num().unwrap() as i64)));
                }
                let got = run(&mut port, &file, proto.get("run").unwrap(), vars);
                let expect = t.get("expect").map(Value::arr).unwrap_or_default();
                assert_eq!(got.len(), expect.len(), "{name} test {i}: emitted {got:?}");
                for (g, e) in got.iter().zip(expect) {
                    let want_pct = e.get("percent").and_then(Value::num).map(|p| p as u8);
                    assert_eq!(g.percent, want_pct, "{name} test {i}: percent, got {g:?}");
                    assert_eq!(g.charging, e.get("charging").is_some_and(|c| matches!(c, Value::Bool(true))), "{name} test {i}: charging, got {g:?}");
                    let want_state = match e.get("state").and_then(Value::str) {
                        Some("asleep") => 1,
                        Some("offline") => 2,
                        _ => 0,
                    };
                    assert_eq!(g.state, want_state, "{name} test {i}: state, got {g:?}");
                    if let Some(slot) = e.get("slot").and_then(Value::str) {
                        assert_eq!(g.slot, slot, "{name} test {i}: slot");
                    }
                    if let Some(sent) = t.get("sent").map(Value::arr) {
                        for (k, s) in sent.iter().enumerate() {
                            let want = hex(s.str().unwrap());
                            assert_eq!(port.sent.get(k).map(|f| &f[..want.len().min(f.len())]), Some(&want[..]), "{name} test {i}: frame {k} sent");
                        }
                    }
                }
                ran += 1;
            }
        }
        println!("{ran} protocol tests passed");
    }
}
