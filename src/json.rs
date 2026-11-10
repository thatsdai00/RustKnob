// Just enough JSON for the two services we talk to (ntfy messages, the upload API).

/// Value of the first string field named `key`. Understands the escapes JSON allows.
pub fn str_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let after_key = &text[text.find(&needle)? + needle.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = after_colon.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => {}
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let mut unit = u32::from_str_radix(&hex, 16).ok()?;
                    // a UTF-16 surrogate pair spans two \u escapes
                    if (0xD800..0xDC00).contains(&unit) {
                        let rest: String = chars.clone().take(6).collect();
                        if let Some(low) = rest.strip_prefix("\\u").and_then(|h| u32::from_str_radix(h, 16).ok()) {
                            chars.by_ref().take(6).for_each(drop);
                            unit = 0x10000 + ((unit - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
                        }
                    }
                    out.push(char::from_u32(unit).unwrap_or('\u{fffd}'));
                }
                other => out.push(other), // \" \\ \/
            },
            c => out.push(c),
        }
    }
    None
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
