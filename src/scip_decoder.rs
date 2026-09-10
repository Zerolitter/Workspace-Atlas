//! Bounded SCIP protobuf decoder (S4 — `SCIP-001`, `SCIP-002`).
//!
//! Schema pinned per `schemas/scip/PROVENANCE.md` (`schemas/scip/scip.proto`,
//! commit `e01e97efac2f6b8c266b4d04825f1f1eab7b8f6c`). Implements a minimal,
//! hand-written protobuf wire-format reader scoped to exactly the SCIP
//! messages Atlas needs — see the provenance doc for why this is not
//! generated via `prost`/`protoc`.
//!
//! Every `DecodeLimits` bound is enforced while decoding, before untrusted
//! provider output can create an Atlas fact. Decoded SCIP types remain
//! internal; `scip_mapping.rs` converts them into
//! `providers::NormalizedBatch`.

use std::fmt;

// ---------------------------------------------------------------------------
// Decoded types (SCIP-shaped, Atlas-internal only)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScipIndex {
    pub metadata: Option<ScipMetadata>,
    pub documents: Vec<ScipDocument>,
    pub external_symbols: Vec<ScipSymbolInformation>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScipMetadata {
    pub tool_name: String,
    pub tool_version: String,
    pub project_root: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PositionEncoding {
    #[default]
    Unspecified,
    Utf8,
    Utf16,
    Utf32,
    Unknown(i32),
}

impl PositionEncoding {
    fn from_scip_value(value: i32) -> Self {
        match value {
            0 => Self::Unspecified,
            1 => Self::Utf8,
            2 => Self::Utf16,
            3 => Self::Utf32,
            other => Self::Unknown(other),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::Utf8 => "utf8",
            Self::Utf16 => "utf16",
            Self::Utf32 => "utf32",
            Self::Unknown(_) => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScipDocument {
    pub language: String,
    pub relative_path: String,
    pub occurrences: Vec<ScipOccurrence>,
    pub symbols: Vec<ScipSymbolInformation>,
    pub position_encoding: PositionEncoding,
}

/// Normalized to the `MultiLineRange` shape regardless of which `oneof`
/// variant (`single_line_range`/`multi_line_range`, or the deprecated
/// `repeated int32 range`) the producer used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScipRange {
    pub start_line: i32,
    pub start_character: i32,
    pub end_line: i32,
    pub end_character: i32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScipOccurrence {
    pub range: ScipRange,
    pub symbol: String,
    pub symbol_roles: i32,
    pub syntax_kind: i32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScipSymbolInformation {
    pub symbol: String,
    pub documentation: Vec<String>,
    pub relationships: Vec<ScipRelationship>,
    pub kind: i32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScipRelationship {
    pub symbol: String,
    pub is_reference: bool,
    pub is_implementation: bool,
    pub is_type_definition: bool,
    pub is_definition: bool,
}

/// `SymbolRole` bitset values (`scip.proto` `enum SymbolRole`).
pub mod symbol_role {
    pub const DEFINITION: i32 = 0x1;
    pub const IMPORT: i32 = 0x2;
    pub const WRITE_ACCESS: i32 = 0x4;
    pub const READ_ACCESS: i32 = 0x8;
    pub const GENERATED: i32 = 0x10;
    pub const TEST: i32 = 0x20;
    pub const FORWARD_DEFINITION: i32 = 0x40;
}

// ---------------------------------------------------------------------------
// Bounds (SCIP-002)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DecodeLimits {
    pub max_documents: usize,
    pub max_occurrences_per_document: usize,
    pub max_symbols_per_document: usize,
    pub max_external_symbols: usize,
    pub max_relationships_per_symbol: usize,
    pub max_documentation_entries_per_symbol: usize,
    pub max_string_bytes: usize,
    pub max_message_bytes: usize,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_documents: 200_000,
            max_occurrences_per_document: 2_000_000,
            max_symbols_per_document: 500_000,
            max_external_symbols: 2_000_000,
            max_relationships_per_symbol: 10_000,
            max_documentation_entries_per_symbol: 1_000,
            max_string_bytes: 4 * 1024 * 1024,
            max_message_bytes: 512 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScipDecodeError {
    Truncated {
        at: usize,
    },
    InvalidVarint {
        at: usize,
    },
    InvalidWireType {
        wire_type: u8,
        at: usize,
    },
    InvalidUtf8 {
        field: &'static str,
    },
    LimitExceeded {
        limit: &'static str,
        value: usize,
        max: usize,
    },
}

impl fmt::Display for ScipDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { at } => write!(f, "truncated protobuf message at byte {at}"),
            Self::InvalidVarint { at } => write!(f, "invalid varint at byte {at}"),
            Self::InvalidWireType { wire_type, at } => {
                write!(f, "unsupported wire type {wire_type} at byte {at}")
            }
            Self::InvalidUtf8 { field } => write!(f, "field {field:?} is not valid UTF-8"),
            Self::LimitExceeded { limit, value, max } => {
                write!(f, "decode limit {limit:?} exceeded: {value} > {max}")
            }
        }
    }
}

impl std::error::Error for ScipDecodeError {}

type DResult<T> = Result<T, ScipDecodeError>;

// ---------------------------------------------------------------------------
// Wire-format reader
// ---------------------------------------------------------------------------

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn has_remaining(&self) -> bool {
        self.pos < self.buf.len()
    }

    fn read_byte(&mut self) -> DResult<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or(ScipDecodeError::Truncated { at: self.pos })?;
        self.pos += 1;
        Ok(b)
    }

    fn read_varint(&mut self) -> DResult<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            if shift >= 70 {
                return Err(ScipDecodeError::InvalidVarint { at: self.pos });
            }
            let byte = self.read_byte()?;
            result |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    fn read_tag(&mut self) -> DResult<Option<(u32, u8)>> {
        if !self.has_remaining() {
            return Ok(None);
        }
        let tag = self.read_varint()?;
        let field_number = (tag >> 3) as u32;
        let wire_type = (tag & 0x7) as u8;
        Ok(Some((field_number, wire_type)))
    }

    fn read_length_delimited(&mut self, max_string_bytes: usize) -> DResult<&'a [u8]> {
        let len = self.read_varint()? as usize;
        if len > max_string_bytes {
            return Err(ScipDecodeError::LimitExceeded {
                limit: "length_delimited_field_bytes",
                value: len,
                max: max_string_bytes,
            });
        }
        let end = self
            .pos
            .checked_add(len)
            .ok_or(ScipDecodeError::Truncated { at: self.pos })?;
        if end > self.buf.len() {
            return Err(ScipDecodeError::Truncated { at: self.pos });
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn read_string(&mut self, field: &'static str, max_string_bytes: usize) -> DResult<String> {
        let bytes = self.read_length_delimited(max_string_bytes)?;
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|_| ScipDecodeError::InvalidUtf8 { field })
    }

    fn read_fixed32(&mut self) -> DResult<u32> {
        if self.pos + 4 > self.buf.len() {
            return Err(ScipDecodeError::Truncated { at: self.pos });
        }
        let bytes: [u8; 4] = self.buf[self.pos..self.pos + 4].try_into().unwrap();
        self.pos += 4;
        Ok(u32::from_le_bytes(bytes))
    }

    fn read_fixed64(&mut self) -> DResult<u64> {
        if self.pos + 8 > self.buf.len() {
            return Err(ScipDecodeError::Truncated { at: self.pos });
        }
        let bytes: [u8; 8] = self.buf[self.pos..self.pos + 8].try_into().unwrap();
        self.pos += 8;
        Ok(u64::from_le_bytes(bytes))
    }

    /// Skip a field's value given its wire type — used for unknown/unhandled
    /// fields so schema evolution never gets silently misinterpreted as a
    /// different field.
    fn skip_field(&mut self, wire_type: u8, max_string_bytes: usize) -> DResult<()> {
        match wire_type {
            0 => {
                self.read_varint()?;
            }
            1 => {
                self.read_fixed64()?;
            }
            2 => {
                self.read_length_delimited(max_string_bytes)?;
            }
            5 => {
                self.read_fixed32()?;
            }
            other => {
                return Err(ScipDecodeError::InvalidWireType {
                    wire_type: other,
                    at: self.pos,
                })
            }
        }
        Ok(())
    }
}

fn zigzag_decode_i32(v: i64) -> i32 {
    ((v >> 1) ^ -(v & 1)) as i32
}

/// SCIP uses plain (non-zigzag) `int32` for range/role/kind fields, so a
/// varint's low 32 bits reinterpreted as signed is correct — protobuf
/// `int32` (not `sint32`) encodes negative numbers as a 10-byte varint of the
/// two's-complement 64-bit value.
fn varint_to_i32(v: u64) -> i32 {
    v as i64 as i32
}

// ---------------------------------------------------------------------------
// Message decoders
// ---------------------------------------------------------------------------

/// Decode a top-level `scip.Index` message. This is the only public decode
/// entry point — `bytes` is the exact content the runtime layer already
/// validated (path-confined, size-capped, hashed) via
/// `provider_runtime::validate_output_artifact`.
pub fn decode_index(bytes: &[u8], limits: &DecodeLimits) -> DResult<ScipIndex> {
    if bytes.len() > limits.max_message_bytes {
        return Err(ScipDecodeError::LimitExceeded {
            limit: "max_message_bytes",
            value: bytes.len(),
            max: limits.max_message_bytes,
        });
    }
    let mut r = Reader::new(bytes);
    let mut index = ScipIndex::default();

    while let Some((field, wire_type)) = r.read_tag()? {
        match (field, wire_type) {
            (1, 2) => {
                let sub = r.read_length_delimited(limits.max_string_bytes.max(4096))?;
                index.metadata = Some(decode_metadata(sub, limits)?);
            }
            (2, 2) => {
                if index.documents.len() >= limits.max_documents {
                    return Err(ScipDecodeError::LimitExceeded {
                        limit: "max_documents",
                        value: index.documents.len() + 1,
                        max: limits.max_documents,
                    });
                }
                let sub = r.read_length_delimited(limits.max_message_bytes)?;
                index.documents.push(decode_document(sub, limits)?);
            }
            (3, 2) => {
                if index.external_symbols.len() >= limits.max_external_symbols {
                    return Err(ScipDecodeError::LimitExceeded {
                        limit: "max_external_symbols",
                        value: index.external_symbols.len() + 1,
                        max: limits.max_external_symbols,
                    });
                }
                let sub = r.read_length_delimited(limits.max_string_bytes)?;
                index
                    .external_symbols
                    .push(decode_symbol_information(sub, limits)?);
            }
            (_, wt) => r.skip_field(wt, limits.max_string_bytes)?,
        }
    }
    Ok(index)
}

fn decode_metadata(bytes: &[u8], limits: &DecodeLimits) -> DResult<ScipMetadata> {
    let mut r = Reader::new(bytes);
    let mut meta = ScipMetadata::default();
    while let Some((field, wire_type)) = r.read_tag()? {
        match (field, wire_type) {
            (2, 2) => {
                let sub = r.read_length_delimited(limits.max_string_bytes)?;
                let (name, version) = decode_tool_info(sub, limits)?;
                meta.tool_name = name;
                meta.tool_version = version;
            }
            (3, 2) => {
                meta.project_root =
                    r.read_string("Metadata.project_root", limits.max_string_bytes)?
            }
            (_, wt) => r.skip_field(wt, limits.max_string_bytes)?,
        }
    }
    Ok(meta)
}

fn decode_tool_info(bytes: &[u8], limits: &DecodeLimits) -> DResult<(String, String)> {
    let mut r = Reader::new(bytes);
    let mut name = String::new();
    let mut version = String::new();
    while let Some((field, wire_type)) = r.read_tag()? {
        match (field, wire_type) {
            (1, 2) => name = r.read_string("ToolInfo.name", limits.max_string_bytes)?,
            (2, 2) => version = r.read_string("ToolInfo.version", limits.max_string_bytes)?,
            (_, wt) => r.skip_field(wt, limits.max_string_bytes)?,
        }
    }
    Ok((name, version))
}

fn decode_document(bytes: &[u8], limits: &DecodeLimits) -> DResult<ScipDocument> {
    let mut r = Reader::new(bytes);
    let mut doc = ScipDocument::default();
    while let Some((field, wire_type)) = r.read_tag()? {
        match (field, wire_type) {
            (1, 2) => {
                doc.relative_path =
                    r.read_string("Document.relative_path", limits.max_string_bytes)?
            }
            (2, 2) => {
                if doc.occurrences.len() >= limits.max_occurrences_per_document {
                    return Err(ScipDecodeError::LimitExceeded {
                        limit: "max_occurrences_per_document",
                        value: doc.occurrences.len() + 1,
                        max: limits.max_occurrences_per_document,
                    });
                }
                let sub = r.read_length_delimited(limits.max_string_bytes)?;
                doc.occurrences.push(decode_occurrence(sub, limits)?);
            }
            (3, 2) => {
                if doc.symbols.len() >= limits.max_symbols_per_document {
                    return Err(ScipDecodeError::LimitExceeded {
                        limit: "max_symbols_per_document",
                        value: doc.symbols.len() + 1,
                        max: limits.max_symbols_per_document,
                    });
                }
                let sub = r.read_length_delimited(limits.max_string_bytes)?;
                doc.symbols.push(decode_symbol_information(sub, limits)?);
            }
            (4, 2) => doc.language = r.read_string("Document.language", limits.max_string_bytes)?,
            (6, 0) => {
                doc.position_encoding = PositionEncoding::from_scip_value(r.read_varint()? as i32)
            }
            (_, wt) => r.skip_field(wt, limits.max_string_bytes)?,
        }
    }
    Ok(doc)
}

fn decode_occurrence(bytes: &[u8], limits: &DecodeLimits) -> DResult<ScipOccurrence> {
    let mut r = Reader::new(bytes);
    let mut occ = ScipOccurrence::default();
    let mut legacy_range: Vec<i32> = Vec::new();
    while let Some((field, wire_type)) = r.read_tag()? {
        match (field, wire_type) {
            // Deprecated packed `repeated int32 range = 1`.
            (1, 2) => {
                let sub = r.read_length_delimited(limits.max_string_bytes)?;
                legacy_range = decode_packed_varints(sub)?;
            }
            (1, 0) => {
                legacy_range.push(varint_to_i32(r.read_varint()?));
            }
            (2, 2) => occ.symbol = r.read_string("Occurrence.symbol", limits.max_string_bytes)?,
            (3, 0) => occ.symbol_roles = varint_to_i32(r.read_varint()?),
            (5, 0) => occ.syntax_kind = varint_to_i32(r.read_varint()?),
            (8, 2) => {
                let sub = r.read_length_delimited(256)?;
                occ.range = decode_single_line_range(sub)?;
            }
            (9, 2) => {
                let sub = r.read_length_delimited(256)?;
                occ.range = decode_multi_line_range(sub)?;
            }
            (_, wt) => r.skip_field(wt, limits.max_string_bytes)?,
        }
    }
    if occ.range == ScipRange::default() && !legacy_range.is_empty() {
        occ.range = range_from_legacy(&legacy_range);
    }
    Ok(occ)
}

fn range_from_legacy(v: &[i32]) -> ScipRange {
    match v.len() {
        3 => ScipRange {
            start_line: v[0],
            start_character: v[1],
            end_line: v[0],
            end_character: v[2],
        },
        4 => ScipRange {
            start_line: v[0],
            start_character: v[1],
            end_line: v[2],
            end_character: v[3],
        },
        _ => ScipRange::default(),
    }
}

fn decode_packed_varints(bytes: &[u8]) -> DResult<Vec<i32>> {
    let mut r = Reader::new(bytes);
    let mut out = Vec::new();
    while r.has_remaining() {
        out.push(varint_to_i32(r.read_varint()?));
    }
    Ok(out)
}

fn decode_single_line_range(bytes: &[u8]) -> DResult<ScipRange> {
    let mut r = Reader::new(bytes);
    let mut line = 0i32;
    let mut start_character = 0i32;
    let mut end_character = 0i32;
    while let Some((field, wire_type)) = r.read_tag()? {
        match (field, wire_type) {
            (1, 0) => line = varint_to_i32(r.read_varint()?),
            (2, 0) => start_character = varint_to_i32(r.read_varint()?),
            (3, 0) => end_character = varint_to_i32(r.read_varint()?),
            (_, wt) => r.skip_field(wt, 4096)?,
        }
    }
    Ok(ScipRange {
        start_line: line,
        start_character,
        end_line: line,
        end_character,
    })
}

fn decode_multi_line_range(bytes: &[u8]) -> DResult<ScipRange> {
    let mut r = Reader::new(bytes);
    let mut range = ScipRange::default();
    while let Some((field, wire_type)) = r.read_tag()? {
        match (field, wire_type) {
            (1, 0) => range.start_line = varint_to_i32(r.read_varint()?),
            (2, 0) => range.start_character = varint_to_i32(r.read_varint()?),
            (3, 0) => range.end_line = varint_to_i32(r.read_varint()?),
            (4, 0) => range.end_character = varint_to_i32(r.read_varint()?),
            (_, wt) => r.skip_field(wt, 4096)?,
        }
    }
    Ok(range)
}

fn decode_symbol_information(
    bytes: &[u8],
    limits: &DecodeLimits,
) -> DResult<ScipSymbolInformation> {
    let mut r = Reader::new(bytes);
    let mut sym = ScipSymbolInformation::default();
    while let Some((field, wire_type)) = r.read_tag()? {
        match (field, wire_type) {
            (1, 2) => {
                sym.symbol = r.read_string("SymbolInformation.symbol", limits.max_string_bytes)?
            }
            (3, 2) => {
                if sym.documentation.len() >= limits.max_documentation_entries_per_symbol {
                    return Err(ScipDecodeError::LimitExceeded {
                        limit: "max_documentation_entries_per_symbol",
                        value: sym.documentation.len() + 1,
                        max: limits.max_documentation_entries_per_symbol,
                    });
                }
                sym.documentation.push(
                    r.read_string("SymbolInformation.documentation", limits.max_string_bytes)?,
                );
            }
            (4, 2) => {
                if sym.relationships.len() >= limits.max_relationships_per_symbol {
                    return Err(ScipDecodeError::LimitExceeded {
                        limit: "max_relationships_per_symbol",
                        value: sym.relationships.len() + 1,
                        max: limits.max_relationships_per_symbol,
                    });
                }
                let sub = r.read_length_delimited(limits.max_string_bytes)?;
                sym.relationships.push(decode_relationship(sub, limits)?);
            }
            (5, 0) => sym.kind = varint_to_i32(r.read_varint()?),
            (_, wt) => r.skip_field(wt, limits.max_string_bytes)?,
        }
    }
    Ok(sym)
}

fn decode_relationship(bytes: &[u8], limits: &DecodeLimits) -> DResult<ScipRelationship> {
    let mut r = Reader::new(bytes);
    let mut rel = ScipRelationship::default();
    while let Some((field, wire_type)) = r.read_tag()? {
        match (field, wire_type) {
            (1, 2) => rel.symbol = r.read_string("Relationship.symbol", limits.max_string_bytes)?,
            (2, 0) => rel.is_reference = r.read_varint()? != 0,
            (3, 0) => rel.is_implementation = r.read_varint()? != 0,
            (4, 0) => rel.is_type_definition = r.read_varint()? != 0,
            (5, 0) => rel.is_definition = r.read_varint()? != 0,
            (_, wt) => r.skip_field(wt, limits.max_string_bytes)?,
        }
    }
    Ok(rel)
}

#[allow(dead_code)]
fn silence_unused_zigzag(v: i64) -> i32 {
    // zigzag_decode_i32 is retained for documentation/future sint32 fields
    // even though the current schema uses plain int32 everywhere Atlas reads.
    zigzag_decode_i32(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Minimal protobuf encoder, test-only, mirrors the wire format the
    // --- decoder above must accept. Kept deliberately separate from the
    // --- decoder implementation so a bug in one is unlikely to be masked by
    // --- the same bug in the other.
    mod encode {
        pub fn varint(mut v: u64, out: &mut Vec<u8>) {
            loop {
                let byte = (v & 0x7F) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(byte);
                    break;
                } else {
                    out.push(byte | 0x80);
                }
            }
        }
        pub fn tag(field: u32, wire_type: u8, out: &mut Vec<u8>) {
            varint(((field as u64) << 3) | wire_type as u64, out);
        }
        pub fn string_field(field: u32, s: &str, out: &mut Vec<u8>) {
            tag(field, 2, out);
            varint(s.len() as u64, out);
            out.extend_from_slice(s.as_bytes());
        }
        pub fn bytes_field(field: u32, bytes: &[u8], out: &mut Vec<u8>) {
            tag(field, 2, out);
            varint(bytes.len() as u64, out);
            out.extend_from_slice(bytes);
        }
        pub fn varint_field(field: u32, v: i64, out: &mut Vec<u8>) {
            tag(field, 0, out);
            varint(v as u64, out);
        }
    }

    fn encode_single_line_range(line: i32, start: i32, end: i32) -> Vec<u8> {
        let mut out = Vec::new();
        encode::varint_field(1, line as i64, &mut out);
        encode::varint_field(2, start as i64, &mut out);
        encode::varint_field(3, end as i64, &mut out);
        out
    }

    fn encode_occurrence(symbol: &str, roles: i32, range: (i32, i32, i32)) -> Vec<u8> {
        let mut out = Vec::new();
        let range_bytes = encode_single_line_range(range.0, range.1, range.2);
        encode::bytes_field(8, &range_bytes, &mut out);
        encode::string_field(2, symbol, &mut out);
        encode::varint_field(3, roles as i64, &mut out);
        out
    }

    fn encode_symbol_information(symbol: &str, docs: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        encode::string_field(1, symbol, &mut out);
        for d in docs {
            encode::string_field(3, d, &mut out);
        }
        out
    }

    fn encode_document(
        relative_path: &str,
        language: &str,
        occurrences: &[Vec<u8>],
        symbols: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        encode::string_field(1, relative_path, &mut out);
        for occ in occurrences {
            encode::bytes_field(2, occ, &mut out);
        }
        for sym in symbols {
            encode::bytes_field(3, sym, &mut out);
        }
        encode::string_field(4, language, &mut out);
        out
    }

    fn encode_tool_info(name: &str, version: &str) -> Vec<u8> {
        let mut out = Vec::new();
        encode::string_field(1, name, &mut out);
        encode::string_field(2, version, &mut out);
        out
    }

    fn encode_metadata(tool_name: &str, tool_version: &str, project_root: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let tool_bytes = encode_tool_info(tool_name, tool_version);
        encode::bytes_field(2, &tool_bytes, &mut out);
        encode::string_field(3, project_root, &mut out);
        out
    }

    fn encode_index(metadata: &[u8], documents: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        encode::bytes_field(1, metadata, &mut out);
        for doc in documents {
            encode::bytes_field(2, doc, &mut out);
        }
        out
    }

    #[test]
    fn decodes_document_position_encoding() {
        let mut utf8_doc = encode_document("src/lib.rs", "rust", &[], &[]);
        encode::varint_field(6, 1, &mut utf8_doc);
        let utf8 = decode_index(&encode_index(&[], &[utf8_doc]), &DecodeLimits::default()).unwrap();
        assert_eq!(utf8.documents[0].position_encoding, PositionEncoding::Utf8);
        let mut utf16_doc = encode_document("src/a.ts", "typescript", &[], &[]);
        encode::varint_field(6, 2, &mut utf16_doc);
        let utf16 =
            decode_index(&encode_index(&[], &[utf16_doc]), &DecodeLimits::default()).unwrap();
        assert_eq!(
            utf16.documents[0].position_encoding,
            PositionEncoding::Utf16
        );
    }

    #[test]
    fn decodes_a_minimal_valid_index() {
        let occ = encode_occurrence(
            "scip-typescript npm mypkg 1.0.0 `src/a.ts`/foo().",
            symbol_role::DEFINITION,
            (0, 4, 7),
        );
        let sym = encode_symbol_information(
            "scip-typescript npm mypkg 1.0.0 `src/a.ts`/foo().",
            &["Does a thing."],
        );
        let doc = encode_document("src/a.ts", "typescript", &[occ], &[sym]);
        let meta = encode_metadata("scip-typescript", "0.4.0", "file:///workspace");
        let index_bytes = encode_index(&meta, &[doc]);

        let limits = DecodeLimits::default();
        let index = decode_index(&index_bytes, &limits).unwrap();

        assert_eq!(
            index.metadata.as_ref().unwrap().tool_name,
            "scip-typescript"
        );
        assert_eq!(index.metadata.as_ref().unwrap().tool_version, "0.4.0");
        assert_eq!(index.documents.len(), 1);
        let d = &index.documents[0];
        assert_eq!(d.relative_path, "src/a.ts");
        assert_eq!(d.language, "typescript");
        assert_eq!(d.occurrences.len(), 1);
        assert_eq!(
            d.occurrences[0].range,
            ScipRange {
                start_line: 0,
                start_character: 4,
                end_line: 0,
                end_character: 7
            }
        );
        assert_eq!(d.occurrences[0].symbol_roles, symbol_role::DEFINITION);
        assert_eq!(d.symbols.len(), 1);
        assert_eq!(
            d.symbols[0].documentation,
            vec!["Does a thing.".to_string()]
        );
    }

    #[test]
    fn decodes_relationships_and_multi_line_range() {
        let mut sub_symbols = Vec::new();
        let mut rel_bytes = Vec::new();
        encode::string_field(1, "Animal#sound().", &mut rel_bytes);
        encode::varint_field(3, 1, &mut rel_bytes); // is_implementation
        let mut sym = Vec::new();
        encode::string_field(1, "Dog#sound().", &mut sym);
        encode::bytes_field(4, &rel_bytes, &mut sym);
        sub_symbols.push(sym);

        let mut multi_range = Vec::new();
        encode::varint_field(1, 10, &mut multi_range); // start_line
        encode::varint_field(2, 2, &mut multi_range); // start_character
        encode::varint_field(3, 12, &mut multi_range); // end_line
        encode::varint_field(4, 5, &mut multi_range); // end_character
        let mut occ = Vec::new();
        encode::bytes_field(9, &multi_range, &mut occ);
        encode::string_field(2, "Dog#sound().", &mut occ);

        let doc = encode_document("src/dog.ts", "typescript", &[occ], &sub_symbols);
        let index_bytes = encode_index(&[], &[doc]);
        let index = decode_index(&index_bytes, &DecodeLimits::default()).unwrap();

        let d = &index.documents[0];
        assert_eq!(
            d.occurrences[0].range,
            ScipRange {
                start_line: 10,
                start_character: 2,
                end_line: 12,
                end_character: 5
            }
        );
        assert_eq!(d.symbols[0].relationships.len(), 1);
        assert!(d.symbols[0].relationships[0].is_implementation);
        assert_eq!(d.symbols[0].relationships[0].symbol, "Animal#sound().");
    }

    #[test]
    fn decodes_deprecated_packed_range_field() {
        let mut occ = Vec::new();
        let mut packed = Vec::new();
        encode::varint(3, &mut packed);
        encode::varint(4, &mut packed);
        encode::varint(9, &mut packed);
        encode::bytes_field(1, &packed, &mut occ);
        encode::string_field(2, "sym", &mut occ);
        let doc = encode_document("a.ts", "typescript", &[occ], &[]);
        let index = decode_index(&encode_index(&[], &[doc]), &DecodeLimits::default()).unwrap();
        assert_eq!(
            index.documents[0].occurrences[0].range,
            ScipRange {
                start_line: 3,
                start_character: 4,
                end_line: 3,
                end_character: 9
            }
        );
    }

    #[test]
    fn rejects_truncated_message() {
        let doc = encode_document("a.ts", "typescript", &[], &[]);
        let mut index_bytes = encode_index(&[], &[doc]);
        index_bytes.truncate(index_bytes.len() - 3);
        let err = decode_index(&index_bytes, &DecodeLimits::default()).unwrap_err();
        assert!(matches!(err, ScipDecodeError::Truncated { .. }));
    }

    #[test]
    fn rejects_invalid_utf8_string() {
        let mut occ = Vec::new();
        encode::bytes_field(2, &[0xFF, 0xFE, 0xFD], &mut occ); // symbol field, invalid UTF-8
        let doc = encode_document("a.ts", "typescript", &[occ], &[]);
        let err = decode_index(&encode_index(&[], &[doc]), &DecodeLimits::default()).unwrap_err();
        assert!(matches!(err, ScipDecodeError::InvalidUtf8 { .. }));
    }

    #[test]
    fn rejects_message_exceeding_max_message_bytes() {
        let doc = encode_document("a.ts", "typescript", &[], &[]);
        let index_bytes = encode_index(&[], &[doc]);
        let limits = DecodeLimits {
            max_message_bytes: 4,
            ..DecodeLimits::default()
        };
        let err = decode_index(&index_bytes, &limits).unwrap_err();
        assert!(matches!(
            err,
            ScipDecodeError::LimitExceeded {
                limit: "max_message_bytes",
                ..
            }
        ));
    }

    #[test]
    fn rejects_too_many_documents() {
        let doc = encode_document("a.ts", "typescript", &[], &[]);
        let index_bytes = encode_index(&[], &[doc.clone(), doc.clone(), doc]);
        let limits = DecodeLimits {
            max_documents: 2,
            ..DecodeLimits::default()
        };
        let err = decode_index(&index_bytes, &limits).unwrap_err();
        assert!(matches!(
            err,
            ScipDecodeError::LimitExceeded {
                limit: "max_documents",
                ..
            }
        ));
    }

    #[test]
    fn rejects_too_many_occurrences_per_document() {
        let occ = encode_occurrence("s", symbol_role::DEFINITION, (0, 0, 1));
        let doc = encode_document("a.ts", "typescript", &[occ.clone(), occ.clone(), occ], &[]);
        let index_bytes = encode_index(&[], &[doc]);
        let limits = DecodeLimits {
            max_occurrences_per_document: 2,
            ..DecodeLimits::default()
        };
        let err = decode_index(&index_bytes, &limits).unwrap_err();
        assert!(matches!(
            err,
            ScipDecodeError::LimitExceeded {
                limit: "max_occurrences_per_document",
                ..
            }
        ));
    }

    #[test]
    fn rejects_oversized_string_field() {
        let huge = "x".repeat(10_000);
        let doc = encode_document(&huge, "typescript", &[], &[]);
        let index_bytes = encode_index(&[], &[doc]);
        let limits = DecodeLimits {
            max_string_bytes: 100,
            ..DecodeLimits::default()
        };
        let err = decode_index(&index_bytes, &limits).unwrap_err();
        assert!(matches!(
            err,
            ScipDecodeError::LimitExceeded {
                limit: "length_delimited_field_bytes",
                ..
            }
        ));
    }

    #[test]
    fn unknown_fields_are_skipped_not_misread() {
        let mut doc = Vec::new();
        encode::string_field(1, "a.ts", &mut doc);
        encode::varint_field(99, 12345, &mut doc); // unknown field, varint
        encode::string_field(100, "unknown string field", &mut doc); // unknown field, length-delimited
        encode::string_field(4, "typescript", &mut doc);
        let index_bytes = encode_index(&[], &[doc]);
        let index = decode_index(&index_bytes, &DecodeLimits::default()).unwrap();
        assert_eq!(index.documents[0].relative_path, "a.ts");
        assert_eq!(index.documents[0].language, "typescript");
    }

    #[test]
    fn empty_input_decodes_to_empty_index() {
        let index = decode_index(&[], &DecodeLimits::default()).unwrap();
        assert_eq!(index, ScipIndex::default());
    }

    #[test]
    fn garbage_bytes_are_rejected_not_silently_accepted() {
        // A byte sequence with a wire type of 6/7 (reserved/invalid) must
        // error rather than be silently ignored.
        let garbage = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x0F]; // tag with wire_type 7
        let err = decode_index(&garbage, &DecodeLimits::default());
        assert!(err.is_err());
    }

    /// The checked fixture is real `scip-typescript@0.4.0` output, not a
    /// hand-encoded protobuf stub.
    #[test]
    fn decodes_real_scip_typescript_fixture_output() {
        let bytes = include_bytes!("../tests/scip_fixtures/semantic_fixture_v1.scip");
        let index = decode_index(bytes, &DecodeLimits::default())
            .unwrap_or_else(|e| panic!("real scip-typescript output must decode: {e}"));

        let meta = index
            .metadata
            .as_ref()
            .expect("real index carries metadata");
        assert_eq!(meta.tool_name, "scip-typescript");
        assert_eq!(meta.tool_version, "0.4.0");
        assert!(
            !index.documents.is_empty(),
            "real fixture must produce at least one document"
        );

        let connection_doc = index
            .documents
            .iter()
            .find(|d| d.relative_path.ends_with("connection.ts"))
            .expect("fixture includes src/controller/connection.ts");
        assert!(!connection_doc.occurrences.is_empty());
        assert!(
            connection_doc
                .symbols
                .iter()
                .any(|s| s.symbol.contains("ConnectionController")),
            "expected a ConnectionController symbol definition in connection.ts, got {:?}",
            connection_doc
                .symbols
                .iter()
                .map(|s| &s.symbol)
                .collect::<Vec<_>>()
        );

        // At least one occurrence must carry the Definition role bit — proves
        // `symbol_roles` bitset decoding is correct against real output, not
        // just the hand-encoded unit fixtures above.
        assert!(index
            .documents
            .iter()
            .flat_map(|d| &d.occurrences)
            .any(|o| o.symbol_roles & symbol_role::DEFINITION != 0));
    }
}
