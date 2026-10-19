use serde_json::Value;

pub(in crate::mcp) fn tool_config(args: &Value) -> Result<String, String> {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("get");

    match action {
        "get" => {}
        "set" => {
            let value = args
                .get("respect_gitignore")
                .and_then(Value::as_bool)
                .ok_or("action=set requires boolean respect_gitignore")?;
            crate::search::set_gitignore_config(Some(value));
        }
        "reset" => crate::search::set_gitignore_config(None),
        _ => return Err("unknown action; use get, set, or reset".to_string()),
    }

    let (respect_gitignore, source) = crate::search::gitignore_config();
    serde_json::to_string_pretty(&serde_json::json!({
        "respect_gitignore": respect_gitignore,
        "source": source,
    }))
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_actions_and_values_without_changing_config() {
        let before = tool_config(&serde_json::json!({})).unwrap();
        assert!(tool_config(&serde_json::json!({ "action": "unknown" })).is_err());
        assert!(tool_config(&serde_json::json!({ "action": "set" })).is_err());
        assert!(tool_config(&serde_json::json!({
            "action": "set",
            "respect_gitignore": "yes"
        }))
        .is_err());
        let after = tool_config(&serde_json::json!({})).unwrap();
        assert_eq!(before, after, "rejected calls must not change config");
    }

    #[test]
    fn get_reports_effective_configuration() {
        let output = tool_config(&serde_json::json!({})).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert!(value["respect_gitignore"].is_boolean());
        assert!(value["source"].is_string());
    }
}
