//! JSON that keeps member order and prints like `JSON.stringify`: what the
//! management commands echo back with `--json`.

#[derive(Debug, Clone, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn parse(text: &str) -> Result<J, String> {
        let mut p = Parser {
            bytes: text.as_bytes(),
            at: 0,
        };
        let value = p.value()?;
        p.skip_ws();
        if p.at != p.bytes.len() {
            return Err("trailing characters".into());
        }
        Ok(value)
    }

    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn items(&self) -> &[J] {
        match self {
            J::Arr(items) => items,
            _ => &[],
        }
    }

    /// `JSON.stringify(value)`.
    pub fn stringify(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => out.push_str(&number(*n)),
            J::Str(s) => string(s, out),
            J::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            J::Obj(members) => {
                out.push('{');
                for (i, (key, value)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    string(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// JavaScript's `Number.prototype.toString` for the values JSON carries.
fn number(n: f64) -> String {
    if !n.is_finite() {
        return "null".into();
    }
    if n == n.trunc() && n.abs() < 1e21 {
        return format!("{}", n as i128);
    }
    let text = format!("{n}");
    // Rust prints 1e-7 as 0.0000001; JS switches to exponent form below 1e-6.
    if n.abs() < 1e-6 || n.abs() >= 1e21 {
        let formatted = format!("{n:e}");
        return match formatted.split_once('e') {
            Some((mantissa, exp)) => {
                let exp: i32 = exp.parse().unwrap_or(0);
                format!(
                    "{mantissa}e{}{}",
                    if exp < 0 { "-" } else { "+" },
                    exp.abs()
                )
            }
            None => text,
        };
    }
    text
}

fn string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.at += 1;
        }
    }

    fn value(&mut self) -> Result<J, String> {
        self.skip_ws();
        match self.bytes.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                let mut members = Vec::new();
                loop {
                    self.skip_ws();
                    if self.bytes.get(self.at) == Some(&b'}') {
                        self.at += 1;
                        return Ok(J::Obj(members));
                    }
                    if !members.is_empty() {
                        self.expect(b',')?;
                        self.skip_ws();
                    }
                    let key = self.string()?;
                    self.skip_ws();
                    self.expect(b':')?;
                    let value = self.value()?;
                    members.push((key, value));
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.bytes.get(self.at) == Some(&b']') {
                        self.at += 1;
                        return Ok(J::Arr(items));
                    }
                    if !items.is_empty() {
                        self.expect(b',')?;
                    }
                    items.push(self.value()?);
                }
            }
            Some(b'"') => Ok(J::Str(self.string()?)),
            Some(b't') => self.word("true", J::Bool(true)),
            Some(b'f') => self.word("false", J::Bool(false)),
            Some(b'n') => self.word("null", J::Null),
            Some(_) => {
                let start = self.at;
                while self
                    .bytes
                    .get(self.at)
                    .is_some_and(|b| matches!(b, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'))
                {
                    self.at += 1;
                }
                std::str::from_utf8(&self.bytes[start..self.at])
                    .ok()
                    .and_then(|t| t.parse::<f64>().ok())
                    .map(J::Num)
                    .ok_or_else(|| "bad number".to_owned())
            }
            None => Err("unexpected end".into()),
        }
    }

    fn word(&mut self, word: &str, value: J) -> Result<J, String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err("bad literal".into())
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected {}", byte as char))
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let Some(&b) = self.bytes.get(self.at) else {
                return Err("unterminated string".into());
            };
            self.at += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(&e) = self.bytes.get(self.at) else {
                        return Err("bad escape".into());
                    };
                    self.at += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let high = self.hex4()?;
                            let c = if (0xd800..0xdc00).contains(&high)
                                && self.bytes[self.at..].starts_with(b"\\u")
                            {
                                self.at += 2;
                                let low = self.hex4()?;
                                char::from_u32(
                                    0x10000 + ((high - 0xd800) << 10) + (low.wrapping_sub(0xdc00)),
                                )
                            } else {
                                char::from_u32(high)
                            };
                            out.push(c.unwrap_or('\u{fffd}'));
                        }
                        _ => return Err("bad escape".into()),
                    }
                }
                _ => {
                    // Copy the UTF-8 sequence this byte starts.
                    let start = self.at - 1;
                    let len = match b {
                        0..=0x7f => 1,
                        0xc0..=0xdf => 2,
                        0xe0..=0xef => 3,
                        _ => 4,
                    };
                    self.at = start + len;
                    out.push_str(
                        std::str::from_utf8(&self.bytes[start..self.at])
                            .map_err(|_| "bad utf-8".to_owned())?,
                    );
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .and_then(|b| std::str::from_utf8(b).ok())
            .and_then(|t| u32::from_str_radix(t, 16).ok())
            .ok_or_else(|| "bad unicode escape".to_owned())?;
        self.at += 4;
        Ok(digits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_order_and_js_number_text_survive() {
        let input = r#"{"b":1.0,"a":[2.5,-0.0000001,1e21,"x\n\u0001"],"c":null}"#;
        let out = J::parse(input).unwrap().stringify();
        assert_eq!(out, r#"{"b":1,"a":[2.5,-1e-7,1e+21,"x\n\u0001"],"c":null}"#);
    }

    #[test]
    fn surrogate_pairs_decode() {
        let value = J::parse(r#""👍""#).unwrap();
        assert_eq!(value, J::Str("👍".into()));
    }
}
