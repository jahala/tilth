use serde_json::Value;

use crate::session::Session;

#[expect(
    clippy::unnecessary_wraps,
    reason = "every tool handler returns Result<String, String> to the dispatcher"
)]
pub(in crate::mcp) fn tool_session(args: &Value, session: &Session) -> Result<String, String> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("summary");
    match action {
        "reset" => {
            session.reset();
            Ok("Session reset.".to_string())
        }
        _ => Ok(session.summary()),
    }
}
