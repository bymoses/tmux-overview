//! Small bounded JSON reader for the subscription cache; no external runtime or Cargo.
use std::collections::BTreeMap;

#[derive(Debug)]
pub(crate) enum Value {
    Null, Bool(bool), Number(f64), String(String), Array(Vec<Value>), Object(BTreeMap<String, Value>),
}
impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> { if let Self::Object(o) = self { o.get(key) } else { None } }
    pub fn number(&self) -> Option<f64> { if let Self::Number(n) = self { Some(*n) } else { None } }
    pub fn string(&self) -> Option<&str> { if let Self::String(s) = self { Some(s) } else { None } }
    pub fn array(&self) -> Option<&[Value]> { if let Self::Array(a) = self { Some(a) } else { None } }
    pub fn boolean(&self) -> bool { matches!(self, Self::Bool(true)) }
}
pub(crate) fn parse(text: &str) -> Option<Value> {
    if text.len() > 128 * 1024 { return None; }
    let mut p = Parser { text, at: 0 };
    let value = p.value(0)?; p.ws();
    (p.at == text.len()).then_some(value)
}
struct Parser<'a> { text: &'a str, at: usize }
impl Parser<'_> {
    fn ws(&mut self) { while self.text.as_bytes().get(self.at).is_some_and(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t')) { self.at += 1; } }
    fn take(&mut self, byte: u8) -> bool { self.ws(); if self.text.as_bytes().get(self.at) == Some(&byte) { self.at += 1; true } else { false } }
    fn literal(&mut self, s: &str, v: Value) -> Option<Value> { if self.text[self.at..].starts_with(s) { self.at += s.len(); Some(v) } else { None } }
    fn hex(&mut self) -> Option<u32> { let s = self.text.get(self.at..self.at + 4)?; let n = u32::from_str_radix(s, 16).ok()?; self.at += 4; Some(n) }
    fn string(&mut self) -> Option<String> {
        if !self.take(b'"') { return None; }
        let mut out = String::new();
        loop {
            let c = self.text[self.at..].chars().next()?; self.at += c.len_utf8();
            match c {
                '"' => return Some(out),
                '\\' => {
                    let esc = *self.text.as_bytes().get(self.at)?; self.at += 1;
                    out.push(match esc {
                        b'"' => '"', b'\\' => '\\', b'/' => '/', b'b' => '\u{8}', b'f' => '\u{c}', b'n' => '\n', b'r' => '\r', b't' => '\t',
                        b'u' => {
                            let mut n = self.hex()?;
                            if (0xd800..=0xdbff).contains(&n) {
                                if !self.text[self.at..].starts_with("\\u") { return None; }
                                self.at += 2; let low = self.hex()?;
                                if !(0xdc00..=0xdfff).contains(&low) { return None; }
                                n = 0x10000 + (n - 0xd800) * 0x400 + low - 0xdc00;
                            }
                            char::from_u32(n)?
                        }
                        _ => return None,
                    });
                }
                c if c < ' ' => return None,
                _ => out.push(c),
            }
        }
    }
    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > 32 { return None; } self.ws();
        match *self.text.as_bytes().get(self.at)? {
            b'"' => self.string().map(Value::String),
            b'{' => {
                self.at += 1; let mut obj = BTreeMap::new();
                if self.take(b'}') { return Some(Value::Object(obj)); }
                loop {
                    let key = self.string()?; if !self.take(b':') { return None; }
                    obj.insert(key, self.value(depth + 1)?);
                    if self.take(b'}') { return Some(Value::Object(obj)); }
                    if !self.take(b',') { return None; }
                }
            }
            b'[' => {
                self.at += 1; let mut values = Vec::new();
                if self.take(b']') { return Some(Value::Array(values)); }
                loop {
                    values.push(self.value(depth + 1)?);
                    if self.take(b']') { return Some(Value::Array(values)); }
                    if !self.take(b',') { return None; }
                }
            }
            b't' => self.literal("true", Value::Bool(true)), b'f' => self.literal("false", Value::Bool(false)), b'n' => self.literal("null", Value::Null),
            _ => {
                let start = self.at; let bytes = self.text.as_bytes();
                if bytes.get(self.at) == Some(&b'-') { self.at += 1; }
                match bytes.get(self.at)? {
                    b'0' => self.at += 1,
                    b'1'..=b'9' => { self.at += 1; while bytes.get(self.at).is_some_and(u8::is_ascii_digit) { self.at += 1; } }
                    _ => return None,
                }
                if bytes.get(self.at) == Some(&b'.') {
                    self.at += 1; let before = self.at;
                    while bytes.get(self.at).is_some_and(u8::is_ascii_digit) { self.at += 1; }
                    if before == self.at { return None; }
                }
                if bytes.get(self.at).is_some_and(|b| matches!(b, b'e' | b'E')) {
                    self.at += 1; if bytes.get(self.at).is_some_and(|b| matches!(b, b'+' | b'-')) { self.at += 1; }
                    let before = self.at; while bytes.get(self.at).is_some_and(u8::is_ascii_digit) { self.at += 1; }
                    if before == self.at { return None; }
                }
                let n: f64 = self.text[start..self.at].parse().ok()?; n.is_finite().then_some(Value::Number(n))
            }
        }
    }
}
