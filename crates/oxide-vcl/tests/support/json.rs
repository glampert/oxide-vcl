//! A small JSON reader for the corpus records (`result.json`, manifests).
//! The crate has no dependencies; this covers what the records use.

#![allow(dead_code)]

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(map) => map.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(number) => Some(*number),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }
}

pub fn parse(text: &str) -> Result<Value, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        at: 0,
    };
    let value = parser.value()?;
    parser.space();
    if parser.at != parser.bytes.len() {
        return Err(format!("trailing data at byte {}", parser.at));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        self.space();
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", byte as char, self.at))
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(format!("bad literal at byte {}", self.at))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.space();
        match self.bytes.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                let mut map = BTreeMap::new();
                self.space();
                if self.bytes.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(Value::Object(map));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.expect(b':')?;
                    map.insert(key, self.value()?);
                    self.space();
                    match self.bytes.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Value::Object(map));
                        }
                        _ => return Err(format!("bad object at byte {}", self.at)),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.bytes.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Value::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    self.space();
                    match self.bytes.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Value::Array(items));
                        }
                        _ => return Err(format!("bad array at byte {}", self.at)),
                    }
                }
            }
            Some(b'"') => self.string().map(Value::String),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(_) => {
                let start = self.at;
                while self.at < self.bytes.len()
                    && matches!(
                        self.bytes[self.at],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.at += 1;
                }
                std::str::from_utf8(&self.bytes[start..self.at])
                    .ok()
                    .and_then(|text| text.parse().ok())
                    .map(Value::Number)
                    .ok_or_else(|| format!("bad number at byte {start}"))
            }
            None => Err("unexpected end".into()),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.at;
            while self.at < self.bytes.len() && !matches!(self.bytes[self.at], b'"' | b'\\') {
                self.at += 1;
            }
            out.push_str(
                std::str::from_utf8(&self.bytes[start..self.at]).map_err(|e| e.to_string())?,
            );
            match self.bytes.get(self.at) {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    let escape = *self.bytes.get(self.at + 1).ok_or("unexpected end")?;
                    self.at += 2;
                    out.push(match escape {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let hex = std::str::from_utf8(&self.bytes[self.at..self.at + 4])
                                .map_err(|e| e.to_string())?;
                            self.at += 4;
                            char::from_u32(u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?)
                                .unwrap_or('\u{fffd}')
                        }
                        other => other as char,
                    });
                }
                _ => return Err("unterminated string".into()),
            }
        }
    }
}
