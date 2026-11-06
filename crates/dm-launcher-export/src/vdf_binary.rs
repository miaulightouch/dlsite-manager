//! Binary VDF as used by Steam's `shortcuts.vdf`.
//!
//! Values are kept in file order and in their original type, so an unmodified
//! document serializes back to the same bytes.

const TYPE_MAP: u8 = 0x00;
const TYPE_STRING: u8 = 0x01;
const TYPE_INT32: u8 = 0x02;
const TYPE_FLOAT32: u8 = 0x03;
const TYPE_POINTER: u8 = 0x04;
const TYPE_WSTRING: u8 = 0x05;
const TYPE_COLOR: u8 = 0x06;
const TYPE_UINT64: u8 = 0x07;
const TYPE_END: u8 = 0x08;
const TYPE_INT64: u8 = 0x0a;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Map(pub Vec<(Vec<u8>, Value)>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Map(Map),
    String(Vec<u8>),
    Int32(u32),
    /// Float, pointer and color: four bytes the exporter never interprets.
    Raw4(u8, [u8; 4]),
    /// Eight-byte integers the exporter never interprets.
    Raw8(u8, [u8; 8]),
    /// Little-endian UTF-16 bytes without the terminator.
    WideString(Vec<u8>),
}

impl Map {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key.as_bytes()))
            .map(|(_, value)| value)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.0
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case(key.as_bytes()))
            .map(|(_, value)| value)
    }

    /// Replace a value in place, keeping the key's existing spelling, or append it.
    pub fn set(&mut self, key: &str, value: Value) {
        match self.get_mut(key) {
            Some(slot) => *slot = value,
            None => self.0.push((key.as_bytes().to_vec(), value)),
        }
    }

    pub fn get_string(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(Value::String(bytes)) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        }
    }
}

pub fn parse(bytes: &[u8]) -> Result<Map, String> {
    let mut reader = Reader { bytes, offset: 0 };
    let map = reader.read_map(0)?;
    if reader.offset != bytes.len() {
        return Err(format!(
            "{} unexpected bytes after the end of the document",
            bytes.len() - reader.offset
        ));
    }
    Ok(map)
}

pub fn serialize(map: &Map) -> Vec<u8> {
    let mut out = Vec::new();
    write_map(&mut out, map);
    out
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], String> {
        let end = self
            .offset
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| format!("truncated value at byte {}", self.offset))?;
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn read_cstring(&mut self) -> Result<Vec<u8>, String> {
        let rest = &self.bytes[self.offset..];
        let length = rest
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| format!("unterminated string at byte {}", self.offset))?;
        let value = rest[..length].to_vec();
        self.offset += length + 1;
        Ok(value)
    }

    fn read_wide_string(&mut self) -> Result<Vec<u8>, String> {
        let mut value = Vec::new();
        loop {
            let unit = self.take(2)?;
            if unit == [0, 0] {
                return Ok(value);
            }
            value.extend_from_slice(unit);
        }
    }

    fn read_map(&mut self, depth: usize) -> Result<Map, String> {
        if depth > MAX_DEPTH {
            return Err("document is nested too deeply".to_owned());
        }
        let mut map = Map::default();
        loop {
            let kind = *self
                .bytes
                .get(self.offset)
                .ok_or_else(|| format!("missing end marker at byte {}", self.offset))?;
            self.offset += 1;
            if kind == TYPE_END {
                return Ok(map);
            }
            let key = self.read_cstring()?;
            let value = match kind {
                TYPE_MAP => Value::Map(self.read_map(depth + 1)?),
                TYPE_STRING => Value::String(self.read_cstring()?),
                TYPE_WSTRING => Value::WideString(self.read_wide_string()?),
                TYPE_INT32 => Value::Int32(u32::from_le_bytes(self.array()?)),
                TYPE_FLOAT32 | TYPE_POINTER | TYPE_COLOR => Value::Raw4(kind, self.array()?),
                TYPE_UINT64 | TYPE_INT64 => Value::Raw8(kind, self.array()?),
                other => {
                    return Err(format!(
                        "unknown value type 0x{other:02x} at byte {}",
                        self.offset - 1
                    ))
                }
            };
            map.0.push((key, value));
        }
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
}

fn write_map(out: &mut Vec<u8>, map: &Map) {
    for (key, value) in &map.0 {
        let kind = match value {
            Value::Map(_) => TYPE_MAP,
            Value::String(_) => TYPE_STRING,
            Value::Int32(_) => TYPE_INT32,
            Value::Raw4(kind, _) | Value::Raw8(kind, _) => *kind,
            Value::WideString(_) => TYPE_WSTRING,
        };
        out.push(kind);
        out.extend_from_slice(key);
        out.push(0);
        match value {
            Value::Map(child) => write_map(out, child),
            Value::String(text) => {
                out.extend_from_slice(text);
                out.push(0);
            }
            Value::Int32(number) => out.extend_from_slice(&number.to_le_bytes()),
            Value::Raw4(_, bytes) => out.extend_from_slice(bytes),
            Value::Raw8(_, bytes) => out.extend_from_slice(bytes),
            Value::WideString(units) => {
                out.extend_from_slice(units);
                out.extend_from_slice(&[0, 0]);
            }
        }
    }
    out.push(TYPE_END);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document with every value type, odd key casing and non-UTF-8 text.
    pub(crate) fn every_type_fixture() -> Vec<u8> {
        let mut doc = Vec::new();
        doc.extend_from_slice(b"\x00shortcuts\x00\x000\x00");
        doc.extend_from_slice(b"\x02appid\x00\x78\x56\x34\xf2");
        doc.extend_from_slice(b"\x01appname\x00Hand made\x00");
        doc.extend_from_slice(b"\x01raw\x00\xff\xfe\x00");
        doc.extend_from_slice(b"\x03float\x00\x00\x00\x80\x3f");
        doc.extend_from_slice(b"\x04ptr\x00\x01\x02\x03\x04");
        doc.extend_from_slice(b"\x05wide\x00a\x00b\x00\x00\x00");
        doc.extend_from_slice(b"\x06color\x00\x11\x22\x33\x44");
        doc.extend_from_slice(b"\x07big\x00\x01\x02\x03\x04\x05\x06\x07\x08");
        doc.extend_from_slice(b"\x0asigned\x00\x08\x07\x06\x05\x04\x03\x02\x01");
        doc.extend_from_slice(b"\x00tags\x00\x010\x00favorite\x00\x08");
        doc.extend_from_slice(b"\x08\x08\x08");
        doc
    }

    #[test]
    fn round_trip_is_byte_exact_for_every_type() {
        let bytes = every_type_fixture();
        let parsed = parse(&bytes).unwrap();
        assert_eq!(serialize(&parsed), bytes);
    }

    #[test]
    fn empty_shortcuts_document_round_trips() {
        let bytes = b"\x00shortcuts\x00\x08\x08";
        assert_eq!(serialize(&parse(bytes).unwrap()), bytes);
    }

    #[test]
    fn lookup_ignores_key_case_and_set_keeps_spelling() {
        let mut map = parse(&every_type_fixture()).unwrap();
        let Some(Value::Map(shortcuts)) = map.get_mut("Shortcuts") else {
            panic!("missing shortcuts");
        };
        let Some(Value::Map(entry)) = shortcuts.get_mut("0") else {
            panic!("missing entry");
        };
        assert_eq!(entry.get_string("AppName").unwrap(), "Hand made");
        entry.set("AppName", Value::String(b"New".to_vec()));
        assert!(entry.0.iter().any(|(key, _)| key == b"appname"));
        assert!(!entry.0.iter().any(|(key, _)| key == b"AppName"));
    }

    #[test]
    fn corrupt_documents_are_rejected() {
        let valid = every_type_fixture();
        for cut in [1, 5, 20, valid.len() - 1] {
            assert!(parse(&valid[..cut]).is_err(), "cut at {cut}");
        }
        assert!(parse(b"\x00shortcuts\x00\x08\x08trailing").is_err());
        assert!(parse(b"\x09weird\x00\x08").is_err());
        assert!(parse(b"plain text, not vdf").is_err());
    }

    #[test]
    fn excessive_nesting_is_rejected_without_overflowing() {
        let mut doc = Vec::new();
        for _ in 0..10_000 {
            doc.extend_from_slice(b"\x00a\x00");
        }
        assert!(parse(&doc).is_err());
    }
}
