// Just enough JSON for the services we talk to (ntfy messages, the upload API, GitHub, the lyrics services).

/// Value of the first string field named `key` anywhere in `text`. Understands the escapes JSON allows.
pub fn str_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut r = Reader { s: text.as_bytes(), i: text.find(&needle)? + needle.len(), depth: 0 };
    if r.peek()? != b':' {
        return None;
    }
    r.i += 1;
    r.string()
}

/// `text` as the inside of a JSON string literal.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Percent-encodes everything but unreserved URL characters.
pub fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A decimal number ("-12", "+500", "23.45", ".5", "1.5e3"), exactly as `str::parse::<f64>` reads the numbers we
/// meet, without the ~18 KB of tables it pulls in. The whole text must be the number.
pub fn number(text: &str) -> Option<f64> {
    let b = text.as_bytes();
    let sign = |i: &mut usize| match b.get(*i) {
        Some(b'-') => {
            *i += 1;
            -1
        }
        Some(b'+') => {
            *i += 1;
            1
        }
        _ => 1,
    };
    let mut i = 0;
    let negative = sign(&mut i) < 0;
    let (mut m, mut exp, mut digits, mut dot) = (0f64, 0i32, 0, false);
    while let Some(&c) = b.get(i) {
        match c {
            b'0'..=b'9' => {
                m = m * 10.0 + f64::from(c - b'0');
                digits += 1;
                exp -= i32::from(dot);
            }
            b'.' if !dot => dot = true,
            _ => break,
        }
        i += 1;
    }
    if digits == 0 {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        let s = sign(&mut i);
        let (mut e, start) = (0i32, i);
        while let Some(&c @ b'0'..=b'9') = b.get(i) {
            e = (e * 10 + i32::from(c - b'0')).min(100_000);
            i += 1;
        }
        if i == start {
            return None;
        }
        exp += s * e;
    }
    if i != b.len() {
        return None;
    }
    // the digits and a power of ten up to 1e22 are exact in an f64: one division or product rounds right
    let v = if exp < 0 { m / 10f64.powi(-exp) } else { m * 10f64.powi(exp) };
    Some(if negative { -v } else { v })
}

/// A parsed JSON value.
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn parse(text: &str) -> Option<Value> {
        Reader { s: text.as_bytes(), i: 0, depth: 0 }.value()
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        if let Value::Str(s) = self { Some(s) } else { None }
    }
    pub fn num(&self) -> Option<f64> {
        if let Value::Num(n) = self { Some(*n) } else { None }
    }
    pub fn arr(&self) -> &[Value] {
        if let Value::Arr(v) = self { v } else { &[] }
    }
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
    depth: u32, // nesting: a hostile response must not run the stack out
}

fn hex(digits: &[u8]) -> Option<u32> {
    u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
}

impl Reader<'_> {
    fn peek(&mut self) -> Option<u8> {
        while self.s.get(self.i)?.is_ascii_whitespace() {
            self.i += 1;
        }
        self.s.get(self.i).copied()
    }

    fn value(&mut self) -> Option<Value> {
        if self.depth > 64 {
            return None;
        }
        self.depth += 1;
        let v = self.value_inner();
        self.depth -= 1;
        v
    }

    fn value_inner(&mut self) -> Option<Value> {
        match self.peek()? {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    match self.peek()? {
                        b'}' => break,
                        b',' => self.i += 1,
                        _ => {
                            let k = self.string()?;
                            if self.peek()? != b':' {
                                return None;
                            }
                            self.i += 1;
                            v.push((k, self.value()?));
                        }
                    }
                }
                self.i += 1;
                Some(Value::Obj(v))
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    match self.peek()? {
                        b']' => break,
                        b',' => self.i += 1,
                        _ => v.push(self.value()?),
                    }
                }
                self.i += 1;
                Some(Value::Arr(v))
            }
            b'"' => self.string().map(Value::Str),
            b't' => {
                self.i += 4;
                Some(Value::Bool(true))
            }
            b'f' => {
                self.i += 5;
                Some(Value::Bool(false))
            }
            b'n' => {
                self.i += 4;
                Some(Value::Null)
            }
            _ => {
                let start = self.i;
                while self.s.get(self.i).is_some_and(|c| matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')) {
                    self.i += 1;
                }
                number(std::str::from_utf8(&self.s[start..self.i]).ok()?).map(Value::Num)
            }
        }
    }

    /// A string literal (after any whitespace), unescaped. An escaped \r is dropped: CR LF comes out as LF.
    fn string(&mut self) -> Option<String> {
        if self.peek()? != b'"' {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => {}
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let mut cp = hex(self.s.get(self.i..self.i + 4)?)?;
                            self.i += 4;
                            // a UTF-16 surrogate pair spans two \u escapes
                            if (0xD800..0xDC00).contains(&cp) && self.s.get(self.i..self.i + 2) == Some(b"\\u") {
                                if let Some(low) = self.s.get(self.i + 2..self.i + 6).and_then(hex) {
                                    self.i += 6;
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
                                }
                            }
                            out.extend_from_slice(char::from_u32(cp).unwrap_or('\u{FFFD}').encode_utf8(&mut [0; 4]).as_bytes());
                        }
                        other => out.push(other), // \" \\ \/
                    }
                }
                other => out.push(other),
            }
        }
        Some(String::from_utf8_lossy(&out).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_match_std() {
        for t in ["0", "-0", "7", "+500", "-1500", "23.45", "01", "59.999", ".5", "5.", "233.0", "12345", "1e3", "1.5E-2", "2e+2", "123456.789", "0.1", "1699999999999.5"] {
            assert_eq!(number(t), t.parse::<f64>().ok(), "{t}");
            assert_eq!(number(t).map(|v| v as f32), t.parse::<f32>().ok(), "{t} as f32");
        }
        for t in ["", ".", "-", "+", "1e", "1e+", "1.2.3", " 1", "1 ", "--1", "1-", "e5", "0x10", "1,5"] {
            assert_eq!(number(t), None, "{t:?}");
            assert!(t.parse::<f64>().is_err(), "{t:?}");
        }
    }

    #[test]
    fn strings() {
        let text = r#"{"a": {"title" : "x\"y\\z\/\n\r\té😀 \ud83d!"}, "title": "second"}"#;
        assert_eq!(str_field(text, "title").as_deref(), Some("x\"y\\z/\n\té😀 \u{FFFD}!"));
        assert_eq!(str_field(text, "missing"), None);
        assert_eq!(str_field(r#"{"k": 5}"#, "k"), None);
        assert_eq!(str_field(r#"{"k": "open"#, "k"), None);
        let v = Value::parse(r#"{"tracks": [{"start": 1.5e3, "on": true, "n": null}]}"#).unwrap();
        let track = &v.get("tracks").unwrap().arr()[0];
        assert_eq!(track.get("start").and_then(Value::num), Some(1500.0));
        assert!(matches!(track.get("on"), Some(Value::Bool(true))));
    }
}
