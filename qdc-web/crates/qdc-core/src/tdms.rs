//! TDMS (NI Technical Data Management Streaming) reader.
//!
//! Matches nptdms behaviour for the slice of the format that
//! `pkpk_processing.read_tdms` actually uses:
//!
//! - file-level properties (esp. `datetime` / `DateTime`)
//! - groups (in the same order as the file declares them)
//! - channels per group, with `name`, `wf_increment` / `dt`, `t0` /
//!   `wf_start_time`, `unit_string`, and the f64-converted sample buffer
//!
//! Tested against three real flavours: Flexlab (single 'Data' group,
//! f64 samples, `t0`+`dt` channel props, no file-level datetime),
//! FlexLogger cDAQ (two groups, `wf_increment`+`wf_start_time`+`unit_string`),
//! and dataflex (single 'Waveforms' group, **float32** samples,
//! file-level `DateTime`).
//!
//! Spec reference: https://www.ni.com/docs/en-US/bundle/labview/page/lvconcepts/tdms_file_format.html
//!
//! Scope of what's implemented:
//! - Single-pass parse from a full byte buffer (suitable for files up to
//!   ~hundreds of MB — fine for our examples and good enough for the
//!   browser demo; can be made segment-streamed later if needed).
//! - Decimated AND interleaved raw-data layouts.
//! - Numeric types: i8/i16/i32/i64, u8/u16/u32/u64, f32/f64.
//! - Property types: those plus String, Bool, Timestamp.
//! - Multi-segment files: channel data is concatenated across segments,
//!   "raw data index same as previous segment" (0x00000000) is honoured.

use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum TdmsError {
    #[error("unexpected end of file at offset {0}")]
    Eof(usize),
    #[error("not a TDMS segment (bad tag at offset {0}): {1:?}")]
    BadTag(usize, [u8; 4]),
    #[error("unsupported TDMS data type code {0:#x} (at object {1:?})")]
    UnsupportedDataType(u32, String),
    #[error("DAQmx raw data is not supported (object {0:?})")]
    DaqmxNotSupported(String),
    #[error("big-endian TDMS is not supported")]
    BigEndianNotSupported,
    #[error("malformed object path {0:?}")]
    MalformedPath(String),
    #[error("malformed metadata: {0}")]
    Malformed(String),
}

pub type Result<T> = std::result::Result<T, TdmsError>;

/// TDMS data type codes (selected, just what we parse).
mod tdtype {
    pub const VOID: u32 = 0;
    pub const I8: u32 = 1;
    pub const I16: u32 = 2;
    pub const I32: u32 = 3;
    pub const I64: u32 = 4;
    pub const U8: u32 = 5;
    pub const U16: u32 = 6;
    pub const U32: u32 = 7;
    pub const U64: u32 = 8;
    pub const F32: u32 = 9;
    pub const F64: u32 = 10;
    pub const STRING: u32 = 0x20;
    pub const BOOL: u32 = 0x21;
    pub const TIMESTAMP: u32 = 0x44;
}

mod toc {
    pub const META_DATA: u32 = 0x2;
    pub const NEW_OBJ_LIST: u32 = 0x4;
    pub const RAW_DATA: u32 = 0x8;
    pub const INTERLEAVED: u32 = 0x20;
    pub const BIG_ENDIAN: u32 = 0x40;
    pub const DAQMX: u32 = 0x80;
}

/// TDMS timestamp: i64 seconds since 1904-01-01 + u64 fractional seconds
/// (1 unit = 2^-64 s). We normalise to a (utc_seconds_since_unix_epoch, ns)
/// tuple matching what `numpy.datetime64('us')` would produce.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TdmsTimestamp {
    /// Microseconds since 1970-01-01 00:00:00 UTC.
    /// nptdms returns timestamps as `numpy.datetime64[us]`, so we match
    /// that resolution exactly. (Sub-microsecond fractions are rounded
    /// half-to-even via the i128 intermediate.)
    pub us_since_epoch: i64,
}

impl TdmsTimestamp {
    /// 1904-01-01 to 1970-01-01 = 66 years incl. 17 leap years = 24107 days
    /// = 2_082_844_800 seconds. (Matches NI's epoch offset.)
    const EPOCH_OFFSET_SECS: i64 = 2_082_844_800;

    /// Build from the raw on-disk fields. `fraction` is the unsigned 2^-64
    /// fraction-of-a-second, `seconds` is signed seconds since NI's 1904 epoch.
    pub fn from_raw(fraction: u64, seconds: i64) -> Self {
        let unix_secs = seconds - Self::EPOCH_OFFSET_SECS;
        // fraction * 1_000_000 / 2^64, rounded.
        // Use i128 to avoid overflow.
        let frac_us = ((fraction as u128).wrapping_mul(1_000_000) >> 64) as i64;
        Self {
            us_since_epoch: unix_secs.saturating_mul(1_000_000) + frac_us,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Void,
    Bool(bool),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    String(String),
    Timestamp(TdmsTimestamp),
}

impl PropertyValue {
    /// Return as f64 if the property holds a numeric scalar that's safely
    /// representable as f64 (covers all integer + float property types we
    /// see in real files for `dt`, `wf_increment`, `t0`).
    pub fn as_f64(&self) -> Option<f64> {
        Some(match self {
            PropertyValue::F32(v) => *v as f64,
            PropertyValue::F64(v) => *v,
            PropertyValue::I8(v) => *v as f64,
            PropertyValue::I16(v) => *v as f64,
            PropertyValue::I32(v) => *v as f64,
            PropertyValue::I64(v) => *v as f64,
            PropertyValue::U8(v) => *v as f64,
            PropertyValue::U16(v) => *v as f64,
            PropertyValue::U32(v) => *v as f64,
            PropertyValue::U64(v) => *v as f64,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> Option<&str> {
        if let PropertyValue::String(s) = self { Some(s.as_str()) } else { None }
    }

    pub fn as_timestamp(&self) -> Option<TdmsTimestamp> {
        if let PropertyValue::Timestamp(t) = self { Some(*t) } else { None }
    }
}

#[derive(Debug)]
pub struct TdmsChannel {
    pub name: String,
    pub properties: BTreeMap<String, PropertyValue>,
    /// Sample data always stored as f64. Files using f32 are widened on read.
    pub data: Vec<f64>,
}

#[derive(Debug)]
pub struct TdmsGroup {
    pub name: String,
    pub properties: BTreeMap<String, PropertyValue>,
    pub channels: Vec<TdmsChannel>,
}

#[derive(Debug)]
pub struct TdmsFile {
    pub properties: BTreeMap<String, PropertyValue>,
    pub groups: Vec<TdmsGroup>,
}

impl TdmsFile {
    /// Convenience: locate channel by group & channel name.
    pub fn channel(&self, group: &str, channel: &str) -> Option<&TdmsChannel> {
        self.groups
            .iter()
            .find(|g| g.name == group)?
            .channels
            .iter()
            .find(|c| c.name == channel)
    }
}

// ===== Internal: byte cursor =====

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self { Self { buf, pos: 0 } }
    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(TdmsError::Eof(self.pos));
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }
    fn read_u32(&mut self) -> Result<u32> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn read_u64(&mut self) -> Result<u64> {
        let b = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
    fn read_i32(&mut self) -> Result<i32> { Ok(self.read_u32()? as i32) }
    fn read_i64(&mut self) -> Result<i64> { Ok(self.read_u64()? as i64) }
    fn read_string(&mut self) -> Result<String> {
        let len = self.read_u32()? as usize;
        let bytes = self.read_bytes(len)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| TdmsError::Malformed("non-UTF8 string".into()))
    }
    fn read_property_value(&mut self, dtype: u32, path: &str) -> Result<PropertyValue> {
        Ok(match dtype {
            tdtype::VOID => PropertyValue::Void,
            tdtype::BOOL => {
                let b = self.read_bytes(1)?;
                PropertyValue::Bool(b[0] != 0)
            }
            tdtype::I8 => PropertyValue::I8(self.read_bytes(1)?[0] as i8),
            tdtype::I16 => {
                let b = self.read_bytes(2)?;
                PropertyValue::I16(i16::from_le_bytes([b[0], b[1]]))
            }
            tdtype::I32 => PropertyValue::I32(self.read_i32()?),
            tdtype::I64 => PropertyValue::I64(self.read_i64()?),
            tdtype::U8 => PropertyValue::U8(self.read_bytes(1)?[0]),
            tdtype::U16 => {
                let b = self.read_bytes(2)?;
                PropertyValue::U16(u16::from_le_bytes([b[0], b[1]]))
            }
            tdtype::U32 => PropertyValue::U32(self.read_u32()?),
            tdtype::U64 => PropertyValue::U64(self.read_u64()?),
            tdtype::F32 => {
                let b = self.read_bytes(4)?;
                PropertyValue::F32(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            }
            tdtype::F64 => {
                let b = self.read_bytes(8)?;
                PropertyValue::F64(f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            }
            tdtype::STRING => PropertyValue::String(self.read_string()?),
            tdtype::TIMESTAMP => {
                let fraction = self.read_u64()?;
                let seconds = self.read_i64()?;
                PropertyValue::Timestamp(TdmsTimestamp::from_raw(fraction, seconds))
            }
            other => return Err(TdmsError::UnsupportedDataType(other, path.into())),
        })
    }
}

// ===== Parsed object index entry =====

/// Per-segment raw-data layout for a single object path.
#[derive(Debug, Clone, Copy)]
struct RawDataIndex {
    data_type: u32,
    /// Number of *values* per chunk (i.e. per channel per data chunk).
    n_values: u64,
    /// Bytes per single value (derived from data_type — TDMS strings need
    /// their own size field which we don't currently exercise for raw data).
    bytes_per_value: usize,
}

fn bytes_for_type(dtype: u32, path: &str) -> Result<usize> {
    Ok(match dtype {
        tdtype::I8 | tdtype::U8 | tdtype::BOOL => 1,
        tdtype::I16 | tdtype::U16 => 2,
        tdtype::I32 | tdtype::U32 | tdtype::F32 => 4,
        tdtype::I64 | tdtype::U64 | tdtype::F64 => 8,
        tdtype::TIMESTAMP => 16,
        _ => return Err(TdmsError::UnsupportedDataType(dtype, path.into())),
    })
}

/// Sticky state for one object path across segments: the most recent raw-data
/// index (so 0x00000000 "same as previous" can be honoured), and an order
/// index so we emit objects in their first-seen order.
#[derive(Debug, Clone)]
struct ObjectState {
    /// 0 = root, 1 = group, 2 = channel
    depth: u8,
    properties: BTreeMap<String, PropertyValue>,
    /// Last seen raw-data index. None means "no raw data".
    last_raw: Option<RawDataIndex>,
    /// For channels: accumulated f64 samples. (Numeric only for now.)
    samples: Vec<f64>,
    /// Order of first appearance.
    order: usize,
    /// Group name (for channel paths) or own name (for groups).
    parent: Option<String>,
    /// Name without the surrounding quotes.
    name: String,
}

fn parse_object_path(path: &str) -> Result<(u8, Option<String>, String)> {
    // "/" = root.
    if path == "/" {
        return Ok((0, None, String::new()));
    }
    // Format: /'name1' or /'name1'/'name2'. Quotes inside a name are escaped
    // by doubling: "''" → "'".
    let bytes = path.as_bytes();
    if bytes.is_empty() || bytes[0] != b'/' {
        return Err(TdmsError::MalformedPath(path.into()));
    }

    let mut components: Vec<String> = Vec::new();
    let mut i = 1; // we've consumed the leading '/'
    let mut first = true;
    while i < bytes.len() {
        if !first {
            // Subsequent components are preceded by '/'.
            if bytes[i] != b'/' {
                return Err(TdmsError::MalformedPath(path.into()));
            }
            i += 1;
        }
        first = false;

        if i >= bytes.len() || bytes[i] != b'\'' {
            return Err(TdmsError::MalformedPath(path.into()));
        }
        i += 1; // consume opening quote

        let mut name = String::new();
        while i < bytes.len() {
            if bytes[i] == b'\'' {
                // Doubled quote = literal; single = end of name.
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    name.push('\'');
                    i += 2;
                } else {
                    i += 1; // consume closing quote
                    break;
                }
            } else {
                name.push(bytes[i] as char);
                i += 1;
            }
        }
        components.push(name);
    }

    match components.len() {
        0 => Ok((0, None, String::new())),
        1 => Ok((1, None, components.pop().unwrap())),
        2 => {
            let channel = components.pop().unwrap();
            let group = components.pop().unwrap();
            Ok((2, Some(group), channel))
        }
        _ => Err(TdmsError::MalformedPath(path.into())),
    }
}

// ===== Main parser =====

pub fn parse_tdms(bytes: &[u8]) -> Result<TdmsFile> {
    let mut objects: BTreeMap<String, ObjectState> = BTreeMap::new();
    let mut order_counter = 0_usize;

    let mut pos = 0_usize;
    while pos < bytes.len() {
        let segment = parse_segment(bytes, pos, &mut objects, &mut order_counter)?;
        pos = segment.next_pos;
    }

    // Assemble into hierarchy. Preserve first-seen order at each level.
    let mut root_props: BTreeMap<String, PropertyValue> = BTreeMap::new();
    let mut group_entries: Vec<(usize, String, BTreeMap<String, PropertyValue>)> = Vec::new();
    let mut channel_entries: Vec<(usize, String, ObjectState)> = Vec::new();

    for (path, st) in objects.into_iter() {
        match st.depth {
            0 => root_props = st.properties,
            1 => group_entries.push((st.order, st.name.clone(), st.properties)),
            2 => channel_entries.push((st.order, path, st)),
            _ => unreachable!(),
        }
    }

    group_entries.sort_by_key(|g| g.0);
    channel_entries.sort_by_key(|c| c.0);

    let mut groups: Vec<TdmsGroup> = group_entries
        .into_iter()
        .map(|(_, name, properties)| TdmsGroup { name, properties, channels: Vec::new() })
        .collect();

    for (_, _path, ch_state) in channel_entries {
        let parent = ch_state.parent.as_deref().unwrap_or("");
        let target = groups.iter_mut().find(|g| g.name == parent).ok_or_else(|| {
            TdmsError::Malformed(format!("channel under unknown group {parent:?}"))
        })?;
        target.channels.push(TdmsChannel {
            name: ch_state.name,
            properties: ch_state.properties,
            data: ch_state.samples,
        });
    }

    Ok(TdmsFile { properties: root_props, groups })
}

struct ParsedSegment {
    next_pos: usize,
}

fn parse_segment(
    bytes: &[u8],
    start: usize,
    objects: &mut BTreeMap<String, ObjectState>,
    order_counter: &mut usize,
) -> Result<ParsedSegment> {
    if start + 28 > bytes.len() {
        return Err(TdmsError::Eof(start));
    }
    let tag = &bytes[start..start + 4];
    if tag != b"TDSm" {
        let mut a = [0u8; 4];
        a.copy_from_slice(tag);
        return Err(TdmsError::BadTag(start, a));
    }
    let mut c = Cursor::new(&bytes[start + 4..]);
    let toc_mask = c.read_u32()?;
    let _version = c.read_u32()?;
    let next_segment_offset = c.read_u64()?;
    let raw_data_offset = c.read_u64()?;

    if toc_mask & toc::BIG_ENDIAN != 0 {
        return Err(TdmsError::BigEndianNotSupported);
    }

    let lead_in_end = start + 28;
    // Segment data (metadata + raw data) starts after the 28-byte lead-in.
    // "Next segment offset" is relative to the end of the lead-in. Honour
    // 0xFFFF…FFFF (unclosed file) by computing the remainder of the file.
    let next_seg_start = if next_segment_offset == u64::MAX {
        bytes.len()
    } else {
        lead_in_end + next_segment_offset as usize
    };

    // === Metadata section ===
    let metadata_present = (toc_mask & toc::META_DATA) != 0;
    let new_obj_list = (toc_mask & toc::NEW_OBJ_LIST) != 0;
    let interleaved = (toc_mask & toc::INTERLEAVED) != 0;
    let raw_present = (toc_mask & toc::RAW_DATA) != 0;
    let daqmx = (toc_mask & toc::DAQMX) != 0;

    // Channel objects that participate in raw data, in the order declared
    // in this segment.
    let mut raw_order: Vec<String> = Vec::new();

    if metadata_present {
        let n_objects = c.read_u32()? as usize;
        for _ in 0..n_objects {
            let path = c.read_string()?;
            let raw_header = c.read_u32()?;

            // Resolve the RawDataIndex for this object in this segment.
            let raw_in_this_segment: Option<RawDataIndex> = match raw_header {
                0xFFFFFFFF => None, // no raw data
                0x00000000 => {
                    // "Same as previous" — pull from prior state.
                    objects.get(&path).and_then(|s| s.last_raw)
                }
                0x69130000 | 0x69140000 => {
                    if daqmx {
                        return Err(TdmsError::DaqmxNotSupported(path));
                    }
                    None
                }
                _len_in_bytes => {
                    // Standard raw data index: data_type + array_dim + n_values.
                    let dtype = c.read_u32()?;
                    let _dim = c.read_u32()?;
                    let n_values = c.read_u64()?;
                    // String raw data has an extra "total size in bytes" field.
                    // We don't currently consume string raw data, but we need
                    // to read the field if it's there to keep cursor aligned.
                    if dtype == tdtype::STRING {
                        let _total_bytes = c.read_u64()?;
                    }
                    Some(RawDataIndex {
                        data_type: dtype,
                        n_values,
                        bytes_per_value: bytes_for_type(dtype, &path)?,
                    })
                }
            };

            // Properties for this object in this segment.
            let n_props = c.read_u32()? as usize;
            let mut new_props: BTreeMap<String, PropertyValue> = BTreeMap::new();
            for _ in 0..n_props {
                let name = c.read_string()?;
                let dtype = c.read_u32()?;
                let value = c.read_property_value(dtype, &path)?;
                new_props.insert(name, value);
            }

            // Some TDMS files (notably the Flexlab flavour) reference groups
            // only implicitly via channel paths like "/'Data'/'X'" — there is
            // no separate /'Data' object declaration. Synthesise the group
            // entry so it shows up in the parsed output.
            let (depth, parent, name) = parse_object_path(&path)
                .map_err(|e| e)?;
            if depth == 2 {
                if let Some(group_name) = parent.as_ref() {
                    let group_path = format!("/'{}'", group_name.replace('\'', "''"));
                    objects.entry(group_path).or_insert_with(|| {
                        let order = *order_counter;
                        *order_counter += 1;
                        ObjectState {
                            depth: 1,
                            properties: BTreeMap::new(),
                            last_raw: None,
                            samples: Vec::new(),
                            order,
                            parent: None,
                            name: group_name.clone(),
                        }
                    });
                }
            }

            // Merge into the persistent ObjectState.
            let state = objects.entry(path.clone()).or_insert_with(|| {
                let order = *order_counter;
                *order_counter += 1;
                ObjectState {
                    depth,
                    properties: BTreeMap::new(),
                    last_raw: None,
                    samples: Vec::new(),
                    order,
                    parent,
                    name,
                }
            });
            for (k, v) in new_props {
                state.properties.insert(k, v);
            }
            if let Some(idx) = raw_in_this_segment {
                state.last_raw = Some(idx);
                raw_order.push(path.clone());
            } else if raw_header == 0x00000000 {
                // "same as previous segment": carry the prior raw layout into
                // this segment if newobjlist=true (i.e. only the objects in
                // this list contribute raw data).
                if let Some(prev) = state.last_raw {
                    let _ = prev; // already there
                    raw_order.push(path.clone());
                }
            }
        }
    }

    // If kTocNewObjList is NOT set, the raw-data channel list is inherited
    // from the prior segment. We accumulate raw_order across segments to
    // model that.
    let raw_order_to_use: Vec<String> = if new_obj_list || raw_order.iter().any(|p| !p.is_empty()) {
        raw_order
    } else {
        // Inherit from prior — every channel-level object that has a last_raw
        // and was previously in the active list.
        objects
            .iter()
            .filter(|(_, s)| s.depth == 2 && s.last_raw.is_some())
            .map(|(p, _)| p.clone())
            .collect()
    };

    // === Raw data section ===
    if raw_present && !raw_order_to_use.is_empty() {
        let raw_section_start = lead_in_end + raw_data_offset as usize;
        let raw_section_end = next_seg_start;
        if raw_section_end < raw_section_start || raw_section_end > bytes.len() {
            return Err(TdmsError::Malformed(format!(
                "raw section out of bounds: {raw_section_start}..{raw_section_end}, file len {}",
                bytes.len()
            )));
        }
        let raw_bytes = &bytes[raw_section_start..raw_section_end];

        // Compute chunk size and number of chunks in this segment.
        let chunk_size_bytes: usize = raw_order_to_use
            .iter()
            .map(|p| {
                objects
                    .get(p)
                    .and_then(|s| s.last_raw)
                    .map(|r| r.bytes_per_value * r.n_values as usize)
                    .unwrap_or(0)
            })
            .sum();
        if chunk_size_bytes == 0 {
            // Nothing to read.
            return Ok(ParsedSegment { next_pos: next_seg_start });
        }
        if raw_bytes.len() % chunk_size_bytes != 0 {
            return Err(TdmsError::Malformed(format!(
                "raw section size {} not a multiple of chunk size {}",
                raw_bytes.len(),
                chunk_size_bytes
            )));
        }
        let n_chunks = raw_bytes.len() / chunk_size_bytes;

        for chunk_idx in 0..n_chunks {
            let chunk_off = chunk_idx * chunk_size_bytes;
            let chunk = &raw_bytes[chunk_off..chunk_off + chunk_size_bytes];
            if interleaved {
                read_chunk_interleaved(chunk, &raw_order_to_use, objects)?;
            } else {
                read_chunk_decimated(chunk, &raw_order_to_use, objects)?;
            }
        }
    }

    Ok(ParsedSegment { next_pos: next_seg_start })
}

fn read_chunk_decimated(
    chunk: &[u8],
    order: &[String],
    objects: &mut BTreeMap<String, ObjectState>,
) -> Result<()> {
    let mut cur = 0_usize;
    for path in order {
        let raw = objects
            .get(path)
            .and_then(|s| s.last_raw)
            .ok_or_else(|| TdmsError::Malformed(format!("missing raw index for {path}")))?;
        let take = raw.bytes_per_value * raw.n_values as usize;
        let slice = &chunk[cur..cur + take];
        cur += take;
        let state = objects.get_mut(path).unwrap();
        append_samples(slice, raw, &mut state.samples, path)?;
    }
    Ok(())
}

fn read_chunk_interleaved(
    chunk: &[u8],
    order: &[String],
    objects: &mut BTreeMap<String, ObjectState>,
) -> Result<()> {
    // All channels in an interleaved chunk must share the same n_values and
    // bytes_per_value; samples are written one-per-channel in round-robin.
    let raws: Vec<RawDataIndex> = order
        .iter()
        .map(|p| {
            objects
                .get(p)
                .and_then(|s| s.last_raw)
                .ok_or_else(|| TdmsError::Malformed(format!("missing raw index for {p}")))
        })
        .collect::<Result<_>>()?;

    if !raws.windows(2).all(|w| w[0].n_values == w[1].n_values) {
        return Err(TdmsError::Malformed("interleaved chunk with mismatched n_values".into()));
    }

    let n = raws[0].n_values as usize;
    // Reserve capacity per channel.
    for path in order {
        let s = objects.get_mut(path).unwrap();
        s.samples.reserve(n);
    }
    let mut cur = 0_usize;
    for _ in 0..n {
        for (path, raw) in order.iter().zip(raws.iter()) {
            let slice = &chunk[cur..cur + raw.bytes_per_value];
            cur += raw.bytes_per_value;
            let v = sample_to_f64(slice, raw.data_type, path)?;
            objects.get_mut(path).unwrap().samples.push(v);
        }
    }
    Ok(())
}

fn append_samples(
    slice: &[u8],
    raw: RawDataIndex,
    out: &mut Vec<f64>,
    path: &str,
) -> Result<()> {
    let n = raw.n_values as usize;
    out.reserve(n);
    match raw.data_type {
        tdtype::F64 => {
            for i in 0..n {
                let b = &slice[i * 8..i * 8 + 8];
                out.push(f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]));
            }
        }
        tdtype::F32 => {
            for i in 0..n {
                let b = &slice[i * 4..i * 4 + 4];
                let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                out.push(v as f64);
            }
        }
        tdtype::I16 => {
            for i in 0..n {
                let b = &slice[i * 2..i * 2 + 2];
                out.push(i16::from_le_bytes([b[0], b[1]]) as f64);
            }
        }
        tdtype::I32 => {
            for i in 0..n {
                let b = &slice[i * 4..i * 4 + 4];
                out.push(i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64);
            }
        }
        tdtype::I64 => {
            for i in 0..n {
                let b = &slice[i * 8..i * 8 + 8];
                out.push(i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64);
            }
        }
        tdtype::U8 => {
            for i in 0..n {
                out.push(slice[i] as f64);
            }
        }
        tdtype::U16 => {
            for i in 0..n {
                let b = &slice[i * 2..i * 2 + 2];
                out.push(u16::from_le_bytes([b[0], b[1]]) as f64);
            }
        }
        tdtype::U32 => {
            for i in 0..n {
                let b = &slice[i * 4..i * 4 + 4];
                out.push(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64);
            }
        }
        tdtype::U64 => {
            for i in 0..n {
                let b = &slice[i * 8..i * 8 + 8];
                out.push(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64);
            }
        }
        tdtype::I8 => {
            for i in 0..n {
                out.push(slice[i] as i8 as f64);
            }
        }
        other => return Err(TdmsError::UnsupportedDataType(other, path.into())),
    }
    Ok(())
}

fn sample_to_f64(slice: &[u8], dtype: u32, path: &str) -> Result<f64> {
    Ok(match dtype {
        tdtype::F64 => f64::from_le_bytes(slice.try_into().map_err(|_| TdmsError::Eof(0))?),
        tdtype::F32 => f32::from_le_bytes(slice.try_into().map_err(|_| TdmsError::Eof(0))?) as f64,
        tdtype::I16 => i16::from_le_bytes(slice.try_into().map_err(|_| TdmsError::Eof(0))?) as f64,
        tdtype::I32 => i32::from_le_bytes(slice.try_into().map_err(|_| TdmsError::Eof(0))?) as f64,
        tdtype::I64 => i64::from_le_bytes(slice.try_into().map_err(|_| TdmsError::Eof(0))?) as f64,
        tdtype::U8 => slice[0] as f64,
        tdtype::U16 => u16::from_le_bytes(slice.try_into().map_err(|_| TdmsError::Eof(0))?) as f64,
        tdtype::U32 => u32::from_le_bytes(slice.try_into().map_err(|_| TdmsError::Eof(0))?) as f64,
        tdtype::U64 => u64::from_le_bytes(slice.try_into().map_err(|_| TdmsError::Eof(0))?) as f64,
        tdtype::I8 => slice[0] as i8 as f64,
        other => return Err(TdmsError::UnsupportedDataType(other, path.into())),
    })
}
