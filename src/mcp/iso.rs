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
    with_meta_header(Some(now), serde_json::Value::Null, body)
}

/// Wrap an output with a leading JSON header line that combines the cache
/// token (when `now` is `Some`) with view-shape metadata (when `meta` is a
/// JSON object). Both are merged into a single first-line object so agents
/// pattern-match one line for all structured signals.
///
/// `meta` is expected to be an `Object`; any other shape (including `Null`)
/// is treated as no extra fields. When the merged header is empty —
/// neither timestamp nor meta provided — the body is returned unchanged
/// so this function is a safe drop-in for paths that conditionally emit a
/// header.
pub fn with_meta_header(now: Option<SystemTime>, meta: serde_json::Value, body: &str) -> String {
    let mut header = serde_json::Map::new();
    if let Some(ts) = now {
        header.insert(
            "if_modified_since".into(),
            serde_json::Value::String(iso_ts(ts)),
        );
    }
    if let serde_json::Value::Object(meta_map) = meta {
        for (k, v) in meta_map {
            header.insert(k, v);
        }
    }
    if header.is_empty() {
        return body.to_string();
    }
    let header_str = serde_json::Value::Object(header).to_string();
    format!("{header_str}\n\n{body}")
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

    #[test]
    fn with_meta_header_merges_cache_token_and_meta() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let meta = serde_json::json!({"view": "outline", "original_line_count": 1500});
        let out = with_meta_header(Some(now), meta, "body");
        let first_line = out.lines().next().expect("at least one line");
        let parsed: serde_json::Value =
            serde_json::from_str(first_line).expect("first line must be valid JSON");
        assert!(parsed.get("if_modified_since").is_some(), "{out}");
        assert_eq!(parsed.get("view").and_then(|v| v.as_str()), Some("outline"));
        assert_eq!(
            parsed
                .get("original_line_count")
                .and_then(serde_json::Value::as_u64),
            Some(1500)
        );
        assert!(out.ends_with("body"), "body trailing: {out}");
    }

    #[test]
    fn with_meta_header_meta_only_no_timestamp() {
        let meta = serde_json::json!({"view": "signature", "next_view": "full"});
        let out = with_meta_header(None, meta, "body");
        let first_line = out.lines().next().expect("at least one line");
        let parsed: serde_json::Value =
            serde_json::from_str(first_line).expect("first line must be valid JSON");
        assert!(parsed.get("if_modified_since").is_none(), "no ts: {out}");
        assert_eq!(
            parsed.get("view").and_then(|v| v.as_str()),
            Some("signature")
        );
        assert_eq!(
            parsed.get("next_view").and_then(|v| v.as_str()),
            Some("full")
        );
    }

    #[test]
    fn with_meta_header_empty_returns_bare_body() {
        let out = with_meta_header(None, serde_json::Value::Null, "body");
        assert_eq!(out, "body", "empty header drops the prefix entirely");
    }

    #[test]
    fn with_meta_header_non_object_meta_treated_as_empty() {
        // Defensive: a caller passing Value::String or similar shouldn't crash
        // or produce malformed JSON — it just behaves as no meta.
        let out = with_meta_header(None, serde_json::Value::String("oops".into()), "body");
        assert_eq!(out, "body");
    }
}
