//! Return-only adapters. JSON stays the data model; TOON is an encoder, not storage.
//! TOON spec 4.2 (fixtures pinned to spec release 4.2.1), UTF-8, no unsafe code.
//! Traversal borrows the JSON tree. Only the output buffer is allocated by the
//! encoder; adapting a typed Serialize value to TOON also materializes a JSON tree.
use axum::http::HeaderMap;
use serde_json::{Map, Number, Value};
use std::fmt::Write;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    #[default]
    Toon,
    UglifyJson,
    None,
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toon => "toon",
            Self::UglifyJson => "uglify-json",
            Self::None => "none",
        }
    }

    /// Exact, request-local negotiation. Reject ambiguous headers before writes.
    pub fn from_headers(headers: &HeaderMap, default: Self) -> Result<Self, &'static str> {
        let mut values = headers.get_all("minify").iter();
        let Some(value) = values.next() else {
            return Ok(default);
        };
        if values.next().is_some() {
            return Err("minify must occur exactly once");
        }
        match value.as_bytes() {
            b"toon" => Ok(Self::Toon),
            b"uglify-json" => Ok(Self::UglifyJson),
            b"none" => Ok(Self::None),
            _ => Err("minify must be toon, uglify-json, or none"),
        }
    }

    pub fn encode(self, value: &Value) -> Result<String, serde_json::Error> {
        match self {
            // Serialize directly: no pretty-print-then-strip pass, parsed copy,
            // regex, or homegrown JSON escaping/numeric implementation.
            Self::UglifyJson => serde_json::to_string(value),
            Self::None => serde_json::to_string_pretty(value),
            Self::Toon => Ok(toon(value)),
        }
    }
}

pub fn toon(value: &Value) -> String {
    toon_with_options(value, Delimiter::Comma, 2)
}

#[derive(Clone, Copy)]
pub enum Delimiter {
    Comma,
    Tab,
    Pipe,
}

impl Delimiter {
    fn byte(self) -> u8 {
        match self {
            Self::Comma => b',',
            Self::Tab => b'\t',
            Self::Pipe => b'|',
        }
    }
}

/// Supported codec options; the MCP adapter deliberately uses the spec defaults.
pub fn toon_with_options(value: &Value, delimiter: Delimiter, indent: usize) -> String {
    assert!(
        indent > 0 && indent <= 16,
        "indent must be between 1 and 16 spaces"
    );
    let mut encoder = Encoder {
        out: String::with_capacity(512),
        delimiter: delimiter.byte(),
        indent,
    };
    encoder.value(value, None, 0, false);
    encoder.out
}

struct Encoder {
    out: String,
    delimiter: u8,
    indent: usize,
}

fn primitive(v: &Value) -> bool {
    !matches!(v, Value::Array(_) | Value::Object(_))
}

/// Compare directly against the first row as a borrowed recursive schema.
/// Every primitive type can share a column; arrays/empty objects cannot.
fn same_shape(template: &Value, row: &Value) -> bool {
    match (template, row) {
        (Value::Object(a), Value::Object(b)) if !a.is_empty() && a.len() == b.len() => a
            .iter()
            .all(|(k, v)| b.get(k).is_some_and(|r| same_shape(v, r))),
        _ => primitive(template) && primitive(row),
    }
}

impl Encoder {
    fn line(&mut self, depth: usize) {
        self.out.push('\n');
        for _ in 0..depth * self.indent {
            self.out.push(' ');
        }
    }

    fn key(&mut self, key: &str) {
        let mut bytes = key.bytes();
        if bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
        {
            self.out.push_str(key);
        } else {
            self.quoted(key);
        }
    }

    /// Copy long UTF-8 runs in bulk, escaping only ASCII special bytes. Byte
    /// boundaries at ASCII characters are necessarily valid UTF-8 boundaries.
    fn quoted(&mut self, text: &str) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        self.out.push('"');
        let mut start = 0;
        for (i, b) in text.bytes().enumerate() {
            let escape = match b {
                b'"' => "\\\"",
                b'\\' => "\\\\",
                b'\n' => "\\n",
                b'\r' => "\\r",
                b'\t' => "\\t",
                0..=31 => "",
                _ => continue,
            };
            self.out.push_str(&text[start..i]);
            if escape.is_empty() {
                self.out.push_str("\\u00");
                self.out.push(HEX[(b >> 4) as usize] as char);
                self.out.push(HEX[(b & 15) as usize] as char);
            } else {
                self.out.push_str(escape);
            }
            start = i + 1;
        }
        self.out.push_str(&text[start..]);
        self.out.push('"');
    }

    fn scalar(&mut self, value: &Value, root: bool) {
        match value {
            Value::Null => self.out.push_str("null"),
            Value::Bool(v) => self.out.push_str(if *v { "true" } else { "false" }),
            Value::Number(v) => self.number(v),
            Value::String(s) => {
                let quote = s.is_empty()
                    || matches!(s.as_str(), "true" | "false" | "null")
                    || s.starts_with(['-', '#', ' ', '\t'])
                    || s.ends_with([' ', '\t'])
                    || (root && s.starts_with('\u{feff}'))
                    || numeric_like(s.as_bytes())
                    || s.bytes().any(|b| {
                        b < 32
                            || b == self.delimiter
                            || matches!(b, b':' | b'"' | b'\\' | b'[' | b']' | b'{' | b'}')
                    });
                if quote {
                    self.quoted(s);
                } else {
                    self.out.push_str(s);
                }
            }
            _ => unreachable!("scalar called only for primitives"),
        }
    }

    fn number(&mut self, n: &Number) {
        // serde_json's shortest-roundtrip formatter into a stack buffer.
        // Keep i64/u64 exact rather than routing large integers through f64.
        let mut text = NumberBuffer {
            bytes: [0; 64],
            len: 0,
        };
        write!(&mut text, "{n}").expect("JSON number fits in 64 bytes");
        let text = std::str::from_utf8(&text.bytes[..text.len]).expect("ASCII number");
        if n.is_i64() || n.is_u64() {
            self.out.push_str(text);
            return;
        }
        let x = n.as_f64().expect("finite JSON float");
        if x == 0.0 {
            self.out.push('0');
            return;
        }
        let Some((mantissa, exponent)) = text.split_once('e') else {
            self.out.push_str(text.strip_suffix(".0").unwrap_or(text));
            return;
        };
        if !(1e-6..1e21).contains(&x.abs()) {
            self.out
                .push_str(mantissa.strip_suffix(".0").unwrap_or(mantissa));
            self.out.push('e');
            if !exponent.starts_with(['-', '+']) {
                self.out.push('+');
            }
            self.out.push_str(exponent);
            return;
        }
        let mantissa = if let Some(rest) = mantissa.strip_prefix('-') {
            self.out.push('-');
            rest
        } else {
            mantissa
        };
        let exponent: i32 = exponent.parse().expect("formatter exponent");
        let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32 + exponent;
        let mut digits = [0u8; 24];
        let mut len = 0;
        for b in mantissa.bytes().filter(|b| *b != b'.') {
            digits[len] = b;
            len += 1;
        }
        while len > 1 && digits[len - 1] == b'0' {
            len -= 1;
        }
        if point <= 0 {
            self.out.push_str("0.");
            for _ in 0..-point {
                self.out.push('0');
            }
        }
        for (i, b) in digits[..len].iter().enumerate() {
            if point > 0 && i as i32 == point {
                self.out.push('.');
            }
            self.out.push(*b as char);
        }
        for _ in len as i32..point {
            self.out.push('0');
        }
    }

    fn header(&mut self, count: usize, keyed: bool, schema: Option<&Map<String, Value>>) {
        write!(&mut self.out, "[{count}").unwrap();
        if keyed {
            self.out.push(':');
        }
        if self.delimiter != b',' {
            self.out.push(self.delimiter as char);
        }
        self.out.push(']');
        if let Some(schema) = schema {
            self.fields(schema);
        }
        self.out.push(':');
    }

    fn fields(&mut self, schema: &Map<String, Value>) {
        self.out.push('{');
        for (i, (key, value)) in schema.iter().enumerate() {
            if i != 0 {
                self.out.push(self.delimiter as char);
            }
            self.key(key);
            if let Value::Object(nested) = value {
                self.fields(nested);
            }
        }
        self.out.push('}');
    }

    fn cells(&mut self, schema: &Map<String, Value>, row: &Map<String, Value>, first: &mut bool) {
        for (key, template) in schema {
            let value = &row[key];
            if let Value::Object(nested) = template {
                self.cells(nested, value.as_object().expect("validated shape"), first);
            } else {
                if !*first {
                    self.out.push(self.delimiter as char);
                }
                *first = false;
                self.scalar(value, false);
            }
        }
    }

    /// The caller emitted indentation or a list marker already. `depth` is the
    /// logical depth of this value/field, including fields carried after '- '.
    fn value(&mut self, value: &Value, key: Option<&str>, depth: usize, list_item: bool) {
        if let Some(key) = key {
            self.key(key);
        }
        match value {
            Value::Object(map) => {
                let template = map.values().next();
                let keyed = !list_item
                    && map.len() >= 2
                    && template
                        .is_some_and(|t| t.is_object() && map.values().all(|v| same_shape(t, v)));
                if keyed {
                    let schema = template.unwrap().as_object().unwrap();
                    self.header(map.len(), true, Some(schema));
                    for (entry, row) in map {
                        self.line(depth + 1);
                        self.key(entry);
                        self.out.push_str(": ");
                        self.cells(schema, row.as_object().unwrap(), &mut true);
                    }
                } else {
                    if key.is_some() {
                        self.out.push(':');
                    }
                    for (i, (k, v)) in map.iter().enumerate() {
                        if key.is_some() || i != 0 {
                            self.line(depth + usize::from(key.is_some()));
                        }
                        self.value(v, Some(k), depth + usize::from(key.is_some()), false);
                    }
                }
            }
            Value::Array(array) => {
                if array.is_empty() && !list_item {
                    if key.is_some() {
                        self.out.push_str(": ");
                    }
                    self.out.push_str("[]");
                    return;
                }
                let template = array.first();
                let table = !list_item
                    && template
                        .is_some_and(|t| t.is_object() && array.iter().all(|v| same_shape(t, v)));
                if table {
                    let schema = template.unwrap().as_object().unwrap();
                    self.header(array.len(), false, Some(schema));
                    for row in array {
                        self.line(depth + 1);
                        self.cells(schema, row.as_object().unwrap(), &mut true);
                    }
                } else {
                    self.header(array.len(), false, None);
                    if array.iter().all(primitive) {
                        for (i, v) in array.iter().enumerate() {
                            self.out
                                .push(if i == 0 { ' ' } else { self.delimiter as char });
                            self.scalar(v, false);
                        }
                    } else {
                        for v in array {
                            self.line(depth + 1);
                            self.out.push('-');
                            if v.as_object().is_some_and(Map::is_empty) {
                                continue;
                            }
                            self.out.push(' ');
                            // A carried object field lives one level deeper;
                            // a keyless inner array lives at the hyphen depth.
                            self.value(v, None, depth + 1 + usize::from(v.is_object()), true);
                        }
                    }
                }
            }
            _ => {
                if key.is_some() {
                    self.out.push_str(": ");
                }
                self.scalar(value, key.is_none() && !list_item && depth == 0);
            }
        }
    }
}

/// Exact ASCII grammar, not f64 parsing (which accepts a different language).
fn numeric_like(mut s: &[u8]) -> bool {
    if matches!(s.first(), Some(b'+' | b'-')) {
        s = &s[1..];
    }
    let digits = s.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    s = &s[digits..];
    if s.first() == Some(&b'.') {
        s = &s[1..];
        let digits = s.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            return false;
        }
        s = &s[digits..];
    }
    if matches!(s.first(), Some(b'e' | b'E')) {
        s = &s[1..];
        if matches!(s.first(), Some(b'+' | b'-')) {
            s = &s[1..];
        }
        let digits = s.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            return false;
        }
        s = &s[digits..];
    }
    s.is_empty()
}

struct NumberBuffer {
    bytes: [u8; 64],
    len: usize,
}
impl std::fmt::Write for NumberBuffer {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        let dest = self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?;
        dest.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}
