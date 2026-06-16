use serde_json::Value;

use crate::session::Session;

pub(in crate::mcp) fn tool_savings(_args: &Value, session: &Session) -> Result<String, String> {
    let (baseline, saved) = session.savings();
    if baseline == 0 {
        return Ok("No measured reads yet this session.".to_string());
    }
    let pct = saved * 100 / baseline;
    Ok(format!(
        "Saved ~{saved} tokens this session (~{pct}% of {baseline} baseline tokens) \
         vs naive read/grep. Conservative lower bound — re-reads and some paths aren't counted."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_savings_zero_activity_returns_no_reads_message() {
        let session = Session::new();
        let args = serde_json::json!({});
        let out = tool_savings(&args, &session).expect("savings on fresh session");
        assert_eq!(out, "No measured reads yet this session.");
    }

    #[test]
    fn tool_savings_with_recorded_savings_includes_count_and_pct() {
        let session = Session::new();
        // baseline=1000, returned=200, saved=800, pct=80%
        session.record_savings(1000, 200);
        let args = serde_json::json!({});
        let out = tool_savings(&args, &session).expect("savings with activity");
        assert!(
            out.contains("800"),
            "output must contain saved count (800): {out}"
        );
        assert!(
            out.contains("80%"),
            "output must contain percentage (80%): {out}"
        );
        assert!(
            out.contains("1000"),
            "output must contain baseline count (1000): {out}"
        );
    }

    #[test]
    fn tool_savings_with_zero_saved_shows_zero_pct() {
        let session = Session::new();
        // returned == baseline: no savings
        session.record_savings(500, 500);
        let args = serde_json::json!({});
        let out = tool_savings(&args, &session).expect("savings with zero pct");
        assert!(
            out.contains("~0") || out.contains("~0%") || out.contains(" 0%"),
            "output must indicate zero savings: {out}"
        );
    }
}
