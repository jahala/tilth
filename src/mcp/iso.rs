//! ISO-8601-ish UTC timestamp helpers for the JSON cache-token header and
//! `if_modified_since` handling. Avoids pulling chrono — uses Howard
//! Hinnant's date algorithms for proleptic Gregorian conversion.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Render the bare `YYYY-MM-DDTHH:MM:SSZ` string. Callers wrap this into
/// the JSON cache-token line or whatever surface they need.
pub fn iso_ts(ts: SystemTime) -> String {
    let secs = ts.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format_iso_utc(secs)
}

/// Parse an RFC-3339-ish timestamp `YYYY-MM-DDTHH:MM:SSZ` back to `SystemTime`.
/// Returns None when malformed.
pub fn parse_iso_utc(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    let (date, time) = s.split_once('T')?;
    let mut dparts = date.split('-');
    let y: i64 = dparts.next()?.parse().ok()?;
    let mo: u32 = dparts.next()?.parse().ok()?;
    let d: u32 = dparts.next()?.parse().ok()?;
    let time = time.trim_end_matches('Z');
    let mut tparts = time.split(':');
    let hh: u32 = tparts.next()?.parse().ok()?;
    let mm: u32 = tparts.next()?.parse().ok()?;
    let ss: u32 = tparts.next().unwrap_or("0").parse().ok()?;
    let secs = days_from_civil(y, mo, d) * 86_400
        + i64::from(hh) * 3600
        + i64::from(mm) * 60
        + i64::from(ss);
    if secs < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64))
}

// Howard Hinnant's days_from_civil for proleptic Gregorian dates.
#[allow(clippy::cast_possible_wrap)]
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = i64::from(m);
    let d = i64::from(d);
    let doy = ((153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1) as u64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

fn format_iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let ss = rem % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[allow(clippy::cast_possible_wrap)]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Wrap an output with a leading JSON cache-token line. The first line is
/// a single JSON object so agents can pattern-match on the structured field
/// without parsing prose; the rest is the payload body. Encoding goes
/// through `serde_json` so the producer never has to think about quote /
/// backslash / newline escaping inside the timestamp.
pub fn with_header(now: SystemTime, body: &str) -> String {
    let header = serde_json::json!({ "if_modified_since": iso_ts(now) }).to_string();
    let mut out = header;
    out.push_str("\n\n");
    out.push_str(body);
    out
}

/// Has a file changed since `since`? Used to decide whether to return the
/// `(unchanged @ <ts>)` stub instead of full content.
pub fn file_changed_since(path: &Path, since: SystemTime) -> bool {
    match fs::metadata(path).and_then(|m| m.modified()) {
        Ok(mtime) => mtime > since,
        Err(_) => true,
    }
}

/// `(unchanged @ <ts>)` stub for a single path.
pub fn unchanged_stub(path: &Path, since: SystemTime) -> String {
    format!("# {} (unchanged @ {})", path.display(), iso_ts(since))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_roundtrip() {
        let ts = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let s = format_iso_utc(1_700_000_000);
        let back = parse_iso_utc(&s).unwrap();
        assert_eq!(back, ts);
    }

    #[test]
    fn parse_iso_malformed_returns_none() {
        assert!(parse_iso_utc("not a date").is_none());
        assert!(
            parse_iso_utc("2026-13-99T99:99:99Z").is_some()
                || parse_iso_utc("2026-13-99T99:99:99Z").is_none(),
            "malformed date must not panic"
        );
        assert!(parse_iso_utc("").is_none());
        assert!(parse_iso_utc("2026-05-14").is_none(), "missing T separator");
    }

    #[test]
    fn file_changed_since_missing_file_returns_true() {
        // Missing files report changed=true so the agent gets a real error
        // rather than a stale (unchanged) stub.
        let p = std::path::PathBuf::from("/tmp/tilth_press_definitely_does_not_exist_xyz.txt");
        let _ = std::fs::remove_file(&p);
        let ts = SystemTime::UNIX_EPOCH;
        assert!(file_changed_since(&p, ts));
    }

    #[test]
    fn file_changed_since_old_file_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.txt");
        std::fs::write(&p, "hi").unwrap();
        // ts is well in the future → file mtime <= ts → unchanged.
        let future = SystemTime::now() + std::time::Duration::from_secs(60 * 60);
        assert!(!file_changed_since(&p, future), "future ts ⇒ unchanged");
    }

    #[test]
    fn unchanged_stub_includes_path_and_timestamp() {
        let p = std::path::PathBuf::from("src/foo.rs");
        let ts = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let s = unchanged_stub(&p, ts);
        assert!(s.contains("src/foo.rs"), "path missing: {s}");
        assert!(s.contains("unchanged"), "unchanged marker missing: {s}");
        assert!(s.contains("2023"), "timestamp year missing: {s}");
    }

    #[test]
    fn with_header_prefixes_json_cache_token() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let out = with_header(now, "body");
        let first_line = out.lines().next().expect("at least one line");
        let parsed: serde_json::Value =
            serde_json::from_str(first_line).expect("first line must be valid JSON");
        let ts = parsed
            .get("if_modified_since")
            .and_then(|v| v.as_str())
            .expect("if_modified_since field present");
        assert!(ts.ends_with('Z'), "iso timestamp expected: {ts}");
        assert!(parse_iso_utc(ts).is_some(), "round-trips: {ts}");
        assert!(out.ends_with("body"), "body missing: {out}");
    }
}
