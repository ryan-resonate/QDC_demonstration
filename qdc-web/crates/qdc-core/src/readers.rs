//! Text-format readers: CSV, DAT, Processed-CSV.
//!
//! Each reader returns a uniform `LoadedFile` so the processing loop can
//! consume any source without caring about the input format. Time is stored
//! as `i64` nanoseconds since the Unix epoch (matches `datetime64[ns]`).
//! Channels are always widened to f64.
//!
//! Date parsing covers the formats `pkpk_processing` exercises in practice:
//! ISO 8601 (`YYYY-MM-DD HH:MM:SS[.ffffff]`), Australian-style `DD/MM/YYYY`,
//! and `YYYY/MM/DD`. We pick a strategy by inspecting the first non-empty
//! time cell rather than parsing every row twice like pandas does.

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("file is not valid UTF-8")]
    NotUtf8,
    #[error("missing header row")]
    NoHeader,
    #[error("row {row} has {got} columns, need at least {need}")]
    ShortRow { row: usize, got: usize, need: usize },
    #[error("unrecognised datetime {0:?} at row {1}")]
    BadTime(String, usize),
    #[error("non-numeric value {0:?} at row {1}, column {2}")]
    BadNumber(String, usize, usize),
    #[error("file declared zero data rows")]
    Empty,
    #[error("DAT first line not 'Scan started at …': {0:?}")]
    DatBadHeader(String),
}

#[derive(Debug)]
pub struct LoadedFile {
    /// Nanoseconds since 1970-01-01 UTC.
    pub times_ns: Vec<i64>,
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub z: Vec<f64>,
    /// Sample period in seconds (typically 1/fs).
    pub dt: f64,
}

/// Inspect a text file's first row and return its column names. Works for
/// CSV (comma) and DAT-style TSV (tab). Used by the channel-scan UI.
pub fn list_headers(text: &str, delimiter: u8) -> Vec<String> {
    let first = text.lines().next().unwrap_or("");
    split_row(first, delimiter)
        .into_iter()
        .map(|s| s.trim().to_string())
        .collect()
}

fn split_row(line: &str, delimiter: u8) -> Vec<&str> {
    // Simple splitter — no quoted-field support. Real-world QDC CSVs don't
    // quote numeric columns and the time column is a plain ISO timestamp.
    line.split(|c: char| (c as u32) == delimiter as u32).collect()
}

// ===== Date parsing =====

#[derive(Debug, Clone, Copy)]
enum DateFmt {
    IsoSpace,        // YYYY-MM-DD HH:MM:SS[.ffffff]
    IsoT,            // YYYY-MM-DDTHH:MM:SS[.ffffff]
    DayFirstSlash,   // DD/MM/YYYY HH:MM:SS[.ffffff]
    DayFirstDash,    // DD-MM-YYYY HH:MM:SS[.ffffff]
    YmdSlash,        // YYYY/MM/DD HH:MM:SS[.ffffff]
}

fn detect_date_fmt(s: &str) -> Option<DateFmt> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Quick discrimination by separator positions.
    let bytes = s.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    // ISO-T vs ISO-space.
    if bytes[4] == b'-' && bytes[7] == b'-' {
        return Some(if bytes.get(10) == Some(&b'T') { DateFmt::IsoT } else { DateFmt::IsoSpace });
    }
    // DD/MM/YYYY or YYYY/MM/DD.
    if bytes[2] == b'/' && bytes[5] == b'/' {
        return Some(DateFmt::DayFirstSlash);
    }
    if bytes[4] == b'/' && bytes[7] == b'/' {
        return Some(DateFmt::YmdSlash);
    }
    if bytes[2] == b'-' && bytes[5] == b'-' {
        return Some(DateFmt::DayFirstDash);
    }
    None
}

fn parse_date_with(fmt: DateFmt, s: &str) -> Result<i64, ()> {
    let s = s.trim();
    match fmt {
        DateFmt::IsoSpace | DateFmt::IsoT => parse_iso(s),
        DateFmt::DayFirstSlash => parse_three_part(s, b'/', true),
        DateFmt::DayFirstDash => parse_three_part(s, b'-', true),
        DateFmt::YmdSlash => parse_three_part(s, b'/', false),
    }
}

/// Parses `YYYY-MM-DD[ T]HH:MM:SS[.ffffff]`. Returns ns since Unix epoch.
fn parse_iso(s: &str) -> Result<i64, ()> {
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return Err(());
    }
    let year: i32 = std::str::from_utf8(&bytes[0..4]).map_err(|_| ())?.parse().map_err(|_| ())?;
    let month: u32 = std::str::from_utf8(&bytes[5..7]).map_err(|_| ())?.parse().map_err(|_| ())?;
    let day: u32 = std::str::from_utf8(&bytes[8..10]).map_err(|_| ())?.parse().map_err(|_| ())?;
    let hour: u32 = std::str::from_utf8(&bytes[11..13]).map_err(|_| ())?.parse().map_err(|_| ())?;
    let minute: u32 = std::str::from_utf8(&bytes[14..16]).map_err(|_| ())?.parse().map_err(|_| ())?;
    let second: u32 = std::str::from_utf8(&bytes[17..19]).map_err(|_| ())?.parse().map_err(|_| ())?;
    let frac_ns = parse_optional_fraction_ns(&s[19..])?;
    ymdhms_to_ns(year, month, day, hour, minute, second, frac_ns)
}

fn parse_three_part(s: &str, sep: u8, day_first: bool) -> Result<i64, ()> {
    // `12/03/2026 13:45:30[.ffffff]` or `2026/03/12 13:45:30`
    let bytes = s.as_bytes();
    // Locate space (or T) separating date and time.
    let mut t_idx = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b' ' || b == b'T' {
            t_idx = Some(i);
            break;
        }
    }
    let t_idx = t_idx.ok_or(())?;
    let date_part = &s[..t_idx];
    let time_part = &s[t_idx + 1..];

    let parts: Vec<&str> = date_part.split(sep as char).collect();
    if parts.len() != 3 {
        return Err(());
    }
    let (y, m, d) = if day_first {
        let d: u32 = parts[0].parse().map_err(|_| ())?;
        let m: u32 = parts[1].parse().map_err(|_| ())?;
        let y: i32 = parts[2].parse().map_err(|_| ())?;
        (y, m, d)
    } else {
        let y: i32 = parts[0].parse().map_err(|_| ())?;
        let m: u32 = parts[1].parse().map_err(|_| ())?;
        let d: u32 = parts[2].parse().map_err(|_| ())?;
        (y, m, d)
    };

    if time_part.len() < 8 {
        return Err(());
    }
    let tb = time_part.as_bytes();
    let h: u32 = std::str::from_utf8(&tb[0..2]).map_err(|_| ())?.parse().map_err(|_| ())?;
    let mi: u32 = std::str::from_utf8(&tb[3..5]).map_err(|_| ())?.parse().map_err(|_| ())?;
    let s2: u32 = std::str::from_utf8(&tb[6..8]).map_err(|_| ())?.parse().map_err(|_| ())?;
    let frac_ns = parse_optional_fraction_ns(&time_part[8..])?;
    ymdhms_to_ns(y, m, d, h, mi, s2, frac_ns)
}

fn parse_optional_fraction_ns(rest: &str) -> Result<u32, ()> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Ok(0);
    }
    if !rest.starts_with('.') {
        return Ok(0);
    }
    let digits = &rest[1..];
    // Cap to 9 digits (nanoseconds) and pad with zeros if shorter.
    let usable = digits.chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
    if usable.is_empty() {
        return Ok(0);
    }
    let take = usable.len().min(9);
    let n: u32 = usable[..take].parse().map_err(|_| ())?;
    let pad = 9 - take;
    Ok(n * 10u32.pow(pad as u32))
}

/// Days from civil date to 1970-01-01, Howard Hinnant's algorithm.
fn civil_to_days(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y / 400 } else { (y - 399) / 400 };
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let d = day as u64;
    let doy = (153 * if m > 2 { m - 3 } else { m + 9 } + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

fn ymdhms_to_ns(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, frac_ns: u32) -> Result<i64, ()> {
    if mo < 1 || mo > 12 || d < 1 || d > 31 || h > 23 || mi > 59 || s > 60 {
        return Err(());
    }
    let days = civil_to_days(y, mo, d);
    let secs = days * 86_400 + (h as i64) * 3600 + (mi as i64) * 60 + s as i64;
    Ok(secs * 1_000_000_000 + frac_ns as i64)
}

// ===== Generic CSV/TSV reader =====

fn parse_f64(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(f64::NAN)
}

/// Generic loader for tab/comma-delimited tabular files with a header row,
/// a time column, and three numeric channel columns.
pub fn read_tabular(
    bytes: &[u8],
    delimiter: u8,
    time_col: usize,
    x_col: usize,
    y_col: usize,
    z_col: usize,
    skip_lines: usize,
    scale: f64,
) -> Result<LoadedFile, ReadError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::NotUtf8)?;

    let mut iter = text.lines();
    for _ in 0..skip_lines {
        iter.next();
    }
    let _header = iter.next().ok_or(ReadError::NoHeader)?;

    let needed_cols = time_col.max(x_col).max(y_col).max(z_col) + 1;

    // Detect time format from the first non-empty row.
    let mut peek_buf = Vec::with_capacity(64);
    let mut fmt: Option<DateFmt> = None;
    for (i, line) in iter.clone().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let cols = split_row(line, delimiter);
        if cols.len() < needed_cols {
            continue;
        }
        fmt = detect_date_fmt(cols[time_col].trim());
        if fmt.is_some() {
            break;
        }
        if peek_buf.len() < 5 {
            peek_buf.push(i);
        }
    }
    let fmt = fmt.ok_or_else(|| ReadError::BadTime("(no detectable format)".into(), 0))?;

    let mut times = Vec::with_capacity(1024);
    let mut xs = Vec::with_capacity(1024);
    let mut ys = Vec::with_capacity(1024);
    let mut zs = Vec::with_capacity(1024);

    for (row_idx, line) in iter.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let cols = split_row(line, delimiter);
        if cols.len() < needed_cols {
            return Err(ReadError::ShortRow {
                row: row_idx + 1,
                got: cols.len(),
                need: needed_cols,
            });
        }
        let t = parse_date_with(fmt, cols[time_col].trim())
            .map_err(|_| ReadError::BadTime(cols[time_col].trim().to_string(), row_idx + 1))?;
        let x = parse_f64(cols[x_col]) * scale;
        let y = parse_f64(cols[y_col]) * scale;
        let z = parse_f64(cols[z_col]) * scale;
        times.push(t);
        xs.push(replace_nan_zero(x));
        ys.push(replace_nan_zero(y));
        zs.push(replace_nan_zero(z));
    }

    if times.len() < 2 {
        return Err(ReadError::Empty);
    }
    let dt = (times[1] - times[0]) as f64 * 1e-9;
    Ok(LoadedFile { times_ns: times, x: xs, y: ys, z: zs, dt })
}

fn replace_nan_zero(v: f64) -> f64 {
    if v.is_nan() { 0.0 } else { v }
}

/// CSV reader matching `pkpk_processing.read_csv`: comma delimiter, scale × 1000.
pub fn read_csv(
    bytes: &[u8],
    time_col: usize,
    x_col: usize,
    y_col: usize,
    z_col: usize,
) -> Result<LoadedFile, ReadError> {
    read_tabular(bytes, b',', time_col, x_col, y_col, z_col, 0, 1000.0)
}

// ===== DAT reader =====

/// DAT (Bartington style) — first line `"Scan started at HH:MM:SS DD/MM/YYYY"`,
/// then a header row, then tab-separated `Time_s\tX\tY\tZ` rows.
pub fn read_dat(bytes: &[u8]) -> Result<LoadedFile, ReadError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::NotUtf8)?;
    let mut iter = text.lines();
    let header = iter.next().ok_or(ReadError::NoHeader)?;
    let prefix = "Scan started at ";
    let stripped = header.strip_prefix(prefix)
        .ok_or_else(|| ReadError::DatBadHeader(header.into()))?
        .trim();
    // Format: "HH:MM:SS DD/MM/YYYY".
    let mut parts = stripped.split_whitespace();
    let time_part = parts.next().ok_or_else(|| ReadError::DatBadHeader(header.into()))?;
    let date_part = parts.next().ok_or_else(|| ReadError::DatBadHeader(header.into()))?;
    let combined = format!("{date_part} {time_part}");
    let start_ns = parse_three_part(&combined, b'/', true)
        .map_err(|_| ReadError::DatBadHeader(header.into()))?;

    // Second line is the column header. Then data.
    let _column_header = iter.next().ok_or(ReadError::NoHeader)?;

    let mut t_seconds = Vec::with_capacity(1024);
    let mut xs = Vec::with_capacity(1024);
    let mut ys = Vec::with_capacity(1024);
    let mut zs = Vec::with_capacity(1024);
    for (row_idx, line) in iter.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 4 {
            return Err(ReadError::ShortRow { row: row_idx + 1, got: cols.len(), need: 4 });
        }
        let ts: f64 = cols[0]
            .trim()
            .parse()
            .map_err(|_| ReadError::BadNumber(cols[0].into(), row_idx + 1, 0))?;
        let x = parse_f64(cols[1]);
        let y = parse_f64(cols[2]);
        let z = parse_f64(cols[3]);
        t_seconds.push(ts);
        xs.push(replace_nan_zero(x));
        ys.push(replace_nan_zero(y));
        zs.push(replace_nan_zero(z));
    }

    if t_seconds.len() < 2 {
        return Err(ReadError::Empty);
    }

    let dt = t_seconds[1] - t_seconds[0];
    // Time grid: start_ns + i*dt (in ns).
    let dt_ns = (dt * 1e9) as i64;
    let times_ns: Vec<i64> = (0..t_seconds.len() as i64)
        .map(|i| start_ns + i * dt_ns)
        .collect();

    Ok(LoadedFile { times_ns, x: xs, y: ys, z: zs, dt })
}

// ===== Processed CSV reader =====

/// Already-processed CSV (output of a prior run): first line may be a
/// `# pkpkTime: …, lp_freq: …` comment; then a header row with
/// `Time`, `X_PkPk`, `Y_PkPk`, `Z_PkPk` (and optionally XY/XZ/ZY/YZ).
pub fn read_processed_csv(bytes: &[u8]) -> Result<ProcessedCsv, ReadError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::NotUtf8)?;
    let mut iter = text.lines().peekable();

    // Skip optional leading "#" comment line.
    let metadata: String;
    if let Some(line) = iter.peek() {
        if line.starts_with('#') {
            metadata = iter.next().unwrap().trim_start_matches('#').trim().to_string();
        } else {
            metadata = String::new();
        }
    } else {
        return Err(ReadError::NoHeader);
    }

    let header_line = iter.next().ok_or(ReadError::NoHeader)?;
    let headers: Vec<String> = split_row(header_line, b',')
        .into_iter()
        .map(|s| s.trim().to_string())
        .collect();

    let find = |name: &str| headers.iter().position(|h| h == name);
    let time_idx = find("Time").ok_or(ReadError::NoHeader)?;
    let x_idx = find("X_PkPk");
    let y_idx = find("Y_PkPk");
    let z_idx = find("Z_PkPk");
    let xy_idx = find("XY_PkPk");
    let xz_idx = find("XZ_PkPk");
    let yz_idx = find("YZ_PkPk").or_else(|| find("ZY_PkPk"));

    let mut times = Vec::new();
    let mut x = Vec::new();
    let mut y = Vec::new();
    let mut z = Vec::new();
    let mut xy = Vec::new();
    let mut xz = Vec::new();
    let mut yz = Vec::new();

    // Detect time format from the first valid row.
    let mut fmt: Option<DateFmt> = None;
    let body: Vec<&str> = iter.collect();

    for line in &body {
        if line.trim().is_empty() {
            continue;
        }
        let cols = split_row(line, b',');
        if cols.len() <= time_idx {
            continue;
        }
        fmt = detect_date_fmt(cols[time_idx].trim());
        if fmt.is_some() {
            break;
        }
    }
    let fmt = fmt.ok_or_else(|| ReadError::BadTime("processed-csv".into(), 0))?;

    let val_at = |cols: &[&str], idx: Option<usize>| -> f64 {
        idx.and_then(|i| cols.get(i)).map(|s| parse_f64(s)).unwrap_or(f64::NAN)
    };

    for (row_idx, line) in body.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let cols = split_row(line, b',');
        if cols.len() <= time_idx {
            continue;
        }
        let t = parse_date_with(fmt, cols[time_idx].trim()).map_err(|_| {
            ReadError::BadTime(cols[time_idx].trim().to_string(), row_idx + 1)
        })?;
        times.push(t);
        x.push(val_at(&cols, x_idx));
        y.push(val_at(&cols, y_idx));
        z.push(val_at(&cols, z_idx));
        xy.push(val_at(&cols, xy_idx));
        xz.push(val_at(&cols, xz_idx));
        yz.push(val_at(&cols, yz_idx));
    }

    Ok(ProcessedCsv { times_ns: times, x_pkpk: x, y_pkpk: y, z_pkpk: z, xy_pkpk: xy, xz_pkpk: xz, yz_pkpk: yz, metadata })
}

#[derive(Debug)]
pub struct ProcessedCsv {
    pub times_ns: Vec<i64>,
    pub x_pkpk: Vec<f64>,
    pub y_pkpk: Vec<f64>,
    pub z_pkpk: Vec<f64>,
    pub xy_pkpk: Vec<f64>,
    pub xz_pkpk: Vec<f64>,
    pub yz_pkpk: Vec<f64>,
    pub metadata: String,
}

// ===== Channel-inspection helpers =====

/// Headers of a CSV file (first row, split by comma).
pub fn inspect_csv_headers(bytes: &[u8]) -> Result<Vec<String>, ReadError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::NotUtf8)?;
    Ok(list_headers(text, b','))
}

/// DAT files always have X/Y/Z channels in fixed positions, but the second
/// line can sometimes name them. We honour that if it's there, else fall
/// back to ["X", "Y", "Z"].
pub fn inspect_dat_headers(bytes: &[u8]) -> Result<Vec<String>, ReadError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::NotUtf8)?;
    let mut lines = text.lines();
    let _first = lines.next();
    if let Some(second) = lines.next() {
        let parts: Vec<&str> = second
            .split('\t')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if parts.len() >= 4 {
            // parts[0] is "Time_s", parts[1..=3] are the channel names.
            return Ok(vec![parts[1].into(), parts[2].into(), parts[3].into()]);
        }
    }
    Ok(vec!["X".into(), "Y".into(), "Z".into()])
}
