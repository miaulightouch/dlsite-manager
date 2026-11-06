//! JSON documents that launchers own: parse strictly, keep key order, and keep
//! the original bytes when an export changes nothing.
use crate::ExportError;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

/// `None` for a missing or blank file, which launchers treat as empty.
pub(crate) fn parse_document(
    path: &Path,
    bytes: Option<&[u8]>,
) -> Result<Option<Value>, ExportError> {
    let Some(bytes) = bytes.filter(|bytes| !bytes.iter().all(u8::is_ascii_whitespace)) else {
        return Ok(None);
    };
    serde_json::from_slice(bytes)
        .map(Some)
        .map_err(|error| ExportError::Corrupt {
            path: path.to_path_buf(),
            message: format!("invalid JSON: {error}"),
        })
}

pub(crate) fn serialize_document(
    original: Option<&[u8]>,
    document: &Value,
    indent: &[u8],
) -> Vec<u8> {
    if let Some(bytes) = original {
        if serde_json::from_slice::<Value>(bytes).is_ok_and(|parsed| parsed == *document) {
            return bytes.to_vec();
        }
    }
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent);
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    document
        .serialize(&mut serializer)
        .expect("serializing a JSON value to memory cannot fail");
    out
}
