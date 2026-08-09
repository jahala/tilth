use serde_json::Value;

pub(in crate::mcp) fn tool_definitions(edit_mode: bool) -> Vec<Value> {
    let read_desc = include_str!("../../../prompts/tools/read.md");
    let cwd_prop = cwd_property();
    let mut tools = vec![
        serde_json::json!({
            "name": "tilth_search",
            "annotations": { "readOnlyHint": true },
            "description": "Search definitions, usages, text, regex, or callers. DO NOT use for a known file/symbol (tilth_read) or file dependencies (tilth_deps). Batch example: tilth_search(queries: [{query: \"foo\"}, {query: \"bar\", kind: \"symbol\"}], cwd: \"/abs/repo\").",
            "inputSchema": {
                "type": "object",
                "required": ["queries", "cwd"],
                "properties": {
                    "queries": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["query"],
                            "properties": {
                                "query": {"type": "string", "description": "Symbol, text, or regex. Commas split up to five symbols only for any/symbol/callers; use separate entries for mixed terms."},
                                "glob": {"type": "string"},
                                "kind": {"type": "string", "enum": ["any", "symbol", "content", "regex", "callers"]}
                            }
                        },
                        "minItems": 1,
                        "maxItems": 10,
                        "description": "Required batch of 1-10 queries; entry kind/glob overrides top-level. Multiple results get query headers."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Subdirectory only; omit for checkout-wide search."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["any", "symbol", "content", "regex", "callers"],
                        "default": "any",
                        "description": "any (default) merges symbol, content, and caller results; symbol finds definitions/usages; content is literal; regex is regex; callers finds call sites."
                    },
                    "expand": {
                        "type": "number",
                        "default": 2,
                        "description": "Top matches expanded with full definitions or ±10 usage lines (default 2)."
                    },
                    "context": {
                        "type": "string",
                        "description": "Edited file path; boosts nearby matches."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max response tokens."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Glob filter: \"*.rs\" includes; \"!*.test.ts\" excludes; \"*.{go,rs}\" expands braces; \"src/**/*.ts\" matches paths."
                    },
                    "if_modified_since": {
                        "type": "string",
                        "description": "ISO-8601 timestamp; unchanged result files return stubs."
                    },
                    "cwd": cwd_prop.clone()
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_read",
            "annotations": { "readOnlyHint": true },
            "description": read_desc,
            "inputSchema": {
                "type": "object",
                "required": ["paths", "cwd"],
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "Required batch (max 20). Suffixes: `path#n-m`, `path#n` (from line n), `path### Heading`, or `path#symbol`."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "full", "signature", "stripped"],
                        "default": "auto",
                        "description": "auto (default): small files full; large code signatures; large Markdown outline. full forces content; signature forces outline; stripped removes plain comments/debug logs/blank runs and is non-editable."
                    },
                    "if_modified_since": {
                        "type": "string",
                        "description": "ISO-8601 timestamp; unchanged files return stubs."
                    },
                    "cwd": cwd_prop.clone(),
                    "budget": {
                        "type": "number",
                        "description": "Max response tokens."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_list",
            "annotations": { "readOnlyHint": true },
            "description": "List a directory tree with token-size rollups; omit patterns for a project overview. Use only without a search term; otherwise search/read. Example: tilth_list(patterns: [\"*.rs\", \"*.toml\"], cwd: \"/abs/repo\").",
            "inputSchema": {
                "type": "object",
                "required": ["cwd"],
                "properties": {
                    "patterns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "Optional batch (max 20); omit patterns for a project overview."
                    },
                    "depth": {
                        "type": "number",
                        "description": "Maximum directory depth; 1 is top-level."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Tree root directory; defaults to the checkout."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max response tokens."
                    },
                    "cwd": cwd_prop.clone()
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_deps",
            "annotations": { "readOnlyHint": true },
            "description": "Check a file's imports and dependent callers before changing/removing an export or relied-on behavior. DO NOT use for ordinary reads or internal edits. Example: tilth_deps(path: \"src/cache.rs\", cwd: \"/abs/repo\").",
            "inputSchema": {
                "type": "object",
                "required": ["path", "cwd"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File whose blast radius to check."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Dependent-search directory; defaults to the checkout."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max tokens; truncates dependents first."
                    },
                    "cwd": cwd_prop.clone()
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_grok",
            "annotations": { "readOnlyHint": true },
            "description": "Get one symbol's definition, body, signature, docs, callees, callers, siblings, and tests. DO NOT use for concept search or file reads. Example: tilth_grok(target: \"parse_unified_diff\", cwd: \"/abs/repo\").",
            "inputSchema": {
                "type": "object",
                "required": ["target", "cwd"],
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Symbol, `file:line`, or `Type::method`."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Subdirectory to disambiguate; defaults to the checkout."
                    },
                    "full": {
                        "type": "boolean",
                        "default": false,
                        "description": "Widen caller/callee/sibling/test caps from 5/5/8/8 to 50/30/30/30."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max response tokens."
                    },
                    "cwd": cwd_prop.clone()
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_diff",
            "annotations": { "readOnlyHint": true },
            "description": "Structural function-level diff. DO NOT use shell git diff/log. Git sources use the server project; only patch/a/b paths anchor under cwd. Examples: tilth_diff(cwd: \"/abs/repo\"); tilth_diff(source: \"HEAD~1\", cwd: \"/abs/repo\").",
            "inputSchema": {
                "type": "object",
                "required": ["cwd"],
                "properties": {
                    "cwd": cwd_prop.clone(),
                    "source": {
                        "type": "string",
                        "description": "uncommitted (default), staged, or git ref; ignored with a/b, patch, or log."
                    },
                    "scope": {
                        "type": "string",
                        "description": "File suffix or, in overview only, checkout-relative directory; log accepts files only."
                    },
                    "a": {
                        "type": "string",
                        "description": "First file; requires b."
                    },
                    "b": {
                        "type": "string",
                        "description": "Second file; requires a."
                    },
                    "patch": {
                        "type": "string",
                        "description": "Patch-file path instead of git diff."
                    },
                    "log": {
                        "type": "string",
                        "description": "Git range for per-commit summaries, e.g. `HEAD~5..HEAD`."
                    },
                    "search": {
                        "type": "string",
                        "description": "Case-insensitive symbol/file substring filter."
                    },
                    "blast": {
                        "type": "boolean",
                        "default": false,
                        "description": "Warn on callers of changed signatures."
                    },
                    "expand": {
                        "type": "number",
                        "default": 0,
                        "description": "Changed symbols to expand with source."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max response tokens."
                    }
                }
            }
        }),
    ];

    if edit_mode {
        tools.push(serde_json::json!({
            "name": "tilth_write",
            "annotations": { "readOnlyHint": false },
            "description": "Edit after a tagged read. tilth_read prints `[path#TAG]` above `N:content`; copy its TAG and shown 1-based integer lines—NEVER invent either. `edits` contains `{path, tag?, ops}` sections; omit tag only for a new or untaggable file. Ops: replace/delete use `{start,end}`; insert_before/after use `{line}`; prepend/append; block ops use `{at}`; replace_text uses {old,new}, must match once; create_file uses {content}; delete_file; move_file. Block ops span the tree-sitter definition at a line or `#symbol`. Escape JSON content as `\\t`/`\\n`; literal controls fail before the server. Drift 3-way-merges or rejects; re-read a rejected file. Sections are independent. Example: tilth_write(edits: [{path: \"a.rs\", tag: \"1A2B\", ops: [{op: \"delete\", start: 2, end: 2}, {op: \"append\", content: \"x\"}]}], cwd: \"/abs/repo\").",
            "inputSchema": {
                "type": "object",
                "required": ["edits", "cwd"],
                "properties": {
                    "edits": {
                        "type": "array",
                        "description": "Up to 20 `{path, tag?, ops}` sections; copy read tags.",
                        "items": {
                            "type": "object",
                            "required": ["path", "ops"],
                            "properties": {
                                "path": { "type": "string", "description": "Absolute or cwd-relative path." },
                                "tag": { "type": "string", "description": "4-hex whole-file read tag." },
                                "ops": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "required": ["op"],
                                        "oneOf": [
                                            { "required": ["op", "start", "end", "content"], "additionalProperties": false, "properties": { "op": { "const": "replace" }, "start": { "type": "integer", "minimum": 1, "maximum": 4_294_967_295_u32 }, "end": { "type": "integer", "minimum": 1, "maximum": 4_294_967_295_u32 }, "content": { "type": "string" } } },
                                            { "required": ["op", "start", "end"], "additionalProperties": false, "properties": { "op": { "const": "delete" }, "start": { "type": "integer", "minimum": 1, "maximum": 4_294_967_295_u32 }, "end": { "type": "integer", "minimum": 1, "maximum": 4_294_967_295_u32 } } },
                                            { "required": ["op", "line", "content"], "additionalProperties": false, "properties": { "op": { "const": "insert_before" }, "line": { "type": "integer", "minimum": 1, "maximum": 4_294_967_295_u32 }, "content": { "type": "string" } } },
                                            { "required": ["op", "line", "content"], "additionalProperties": false, "properties": { "op": { "const": "insert_after" }, "line": { "type": "integer", "minimum": 1, "maximum": 4_294_967_295_u32 }, "content": { "type": "string" } } },
                                            { "required": ["op", "content"], "additionalProperties": false, "properties": { "op": { "const": "prepend" }, "content": { "type": "string" } } },
                                            { "required": ["op", "content"], "additionalProperties": false, "properties": { "op": { "const": "append" }, "content": { "type": "string" } } },
                                            { "required": ["op", "at", "content"], "additionalProperties": false, "properties": { "op": { "const": "replace_block" }, "at": { "type": ["integer", "string"], "minimum": 1, "maximum": 4_294_967_295_u32 }, "content": { "type": "string" } } },
                                            { "required": ["op", "at"], "additionalProperties": false, "properties": { "op": { "const": "delete_block" }, "at": { "type": ["integer", "string"], "minimum": 1, "maximum": 4_294_967_295_u32 } } },
                                            { "required": ["op", "at", "content"], "additionalProperties": false, "properties": { "op": { "const": "insert_after_block" }, "at": { "type": ["integer", "string"], "minimum": 1, "maximum": 4_294_967_295_u32 }, "content": { "type": "string" } } },
                                            { "required": ["op"], "additionalProperties": false, "properties": { "op": { "const": "delete_file" } } },
                                            { "required": ["op", "dest"], "additionalProperties": false, "properties": { "op": { "const": "move_file" }, "dest": { "type": "string" } } },
                                            { "required": ["op", "old", "new"], "additionalProperties": false, "properties": { "op": { "const": "replace_text" }, "old": { "type": "string", "minLength": 1 }, "new": { "type": "string" } } },
                                            { "required": ["op", "content"], "additionalProperties": false, "properties": { "op": { "const": "create_file" }, "content": { "type": "string" } } }
                                        ]
                                    }
                                }
                            }
                        }
                    },
                    "diff": {
                        "type": "boolean",
                        "default": false,
                        "description": "Include compact diffs per section."
                    },
                    "cwd": cwd_prop.clone()
                }
            }
        }));
    }

    tools
}

/// The shared `cwd` schema property. The description text is model-facing
/// and must always tell the model to set `cwd` explicitly.
fn cwd_property() -> Value {
    serde_json::json!({ "type": "string", "description": "Absolute checkout directory; always set this explicitly. Relative paths/scopes anchor here; absolute paths pass; shell cwd is invisible." })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilth_write_schema_requires_edits_array_of_sections() {
        let tools = tool_definitions(true);
        let write = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_write"))
            .expect("tilth_write tool definition present in edit mode");
        let schema = &write["inputSchema"];
        assert_eq!(schema["required"][0], "edits", "edits array is required");
        assert_eq!(
            schema["properties"]["edits"]["type"], "array",
            "edits is now a JSON array of section objects, not a text blob"
        );
        // Section items require path + ops.
        let item_required: Vec<&str> = schema["properties"]["edits"]["items"]["required"]
            .as_array()
            .expect("section item required list present")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            item_required.contains(&"path") && item_required.contains(&"ops"),
            "each section must require path and ops: {item_required:?}"
        );
        // The ops oneOf must name every one of the 13 verbs via `op` const.
        let ops_item = &schema["properties"]["edits"]["items"]["properties"]["ops"]["items"];
        let branches = ops_item["oneOf"].as_array().expect("ops oneOf present");
        let verbs: Vec<&str> = branches
            .iter()
            .filter_map(|b| b["properties"]["op"]["const"].as_str())
            .collect();
        for verb in [
            "replace",
            "delete",
            "insert_before",
            "insert_after",
            "prepend",
            "append",
            "replace_block",
            "delete_block",
            "insert_after_block",
            "delete_file",
            "move_file",
            "replace_text",
            "create_file",
        ] {
            assert!(
                verbs.contains(&verb),
                "ops oneOf must name '{verb}': {verbs:?}"
            );
        }
        assert_eq!(
            branches.len(),
            13,
            "exactly 13 verbs expected in the ops oneOf: {verbs:?}"
        );
        // The old per-file `files` array surface stays gone.
        assert!(
            schema["properties"].get("files").is_none(),
            "the per-file `files` array must not reappear"
        );
    }

    /// Compile the full inputSchema and exercise it end-to-end: `{}` fails
    /// (edits missing), a valid section array passes, and an op object missing a
    /// required field is rejected at the schema layer before any file work.
    #[test]
    fn tilth_write_schema_validates_ops_and_rejects_bad_op() {
        let tools = tool_definitions(true);
        let write = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_write"))
            .expect("tilth_write tool definition present in edit mode");
        let compiled = jsonschema::JSONSchema::compile(&write["inputSchema"])
            .expect("tilth_write inputSchema must be a valid JSON Schema");

        assert!(
            !compiled.is_valid(&serde_json::json!({})),
            "empty args must fail: edits is required"
        );
        let valid = serde_json::json!({
            "edits": [{
                "path": "a.rs",
                "tag": "1A2B",
                "ops": [{ "op": "replace", "start": 1, "end": 2, "content": "x" }]
            }],
            "cwd": "/abs",
            "diff": true
        });
        assert!(
            compiled.is_valid(&valid),
            "a valid section array must validate"
        );
        // `replace` missing its `content` field must be rejected by the oneOf.
        let bad = serde_json::json!({
            "edits": [{ "path": "a.rs", "ops": [{ "op": "replace", "start": 1, "end": 2 }] }]
        });
        assert!(
            !compiled.is_valid(&bad),
            "a replace op missing `content` must fail schema validation"
        );
        // Boundary: a negative line number must be rejected by `minimum: 1`.
        let negative = serde_json::json!({
            "edits": [{ "path": "a.rs", "ops": [{ "op": "replace", "start": -1, "end": 2, "content": "x" }] }]
        });
        assert!(
            !compiled.is_valid(&negative),
            "a negative start must fail schema validation (minimum: 1)"
        );
        // Boundary: line 0 is 1-based-invalid at runtime (`check_bounds` rejects
        // `line < 1`), so the schema must reject it too — not defer to a late error.
        let zero = serde_json::json!({
            "edits": [{ "path": "a.rs", "ops": [{ "op": "replace", "start": 0, "end": 2, "content": "x" }] }]
        });
        assert!(
            !compiled.is_valid(&zero),
            "start 0 must fail schema validation (minimum: 1, matching runtime check_bounds)"
        );
        // An op carrying a field foreign to its variant must fail the schema, so a
        // client validating client-side sees the same rejection `deny_unknown_fields`
        // gives server-side (no schema-valid-but-runtime-rejected round-trip).
        let extra_field = serde_json::json!({
            "edits": [{ "path": "a.rs", "ops": [{ "op": "delete", "start": 1, "end": 2, "content": "oops" }] }]
        });
        assert!(
            !compiled.is_valid(&extra_field),
            "a delete op with a stray `content` must fail schema validation (additionalProperties: false)"
        );
        // Boundary: a line number above u32::MAX must be rejected by `maximum`.
        let too_big = serde_json::json!({
            "edits": [{ "path": "a.rs", "ops": [{ "op": "delete", "start": 1, "end": 4_294_967_296_u64 }] }]
        });
        assert!(
            !compiled.is_valid(&too_big),
            "an end above u32::MAX must fail schema validation (maximum: 4294967295)"
        );
    }

    /// `tilth_search` schema must stay aligned with the runtime: `any` is a
    /// valid `kind` (top-level + per-entry), `any` is the default, and the
    /// root requires `queries` so `{}` (and the dropped singular `query`) are
    /// rejected client-side.
    #[test]
    fn tilth_search_schema_matches_runtime_kind_and_requires_a_query() {
        let tools = tool_definitions(false);
        let search = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_search"))
            .expect("tilth_search tool definition present");
        let schema = &search["inputSchema"];

        let kind = &schema["properties"]["kind"];
        let kind_enum: Vec<&str> = kind["enum"]
            .as_array()
            .expect("kind enum present")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            kind_enum.contains(&"any"),
            "top-level kind enum must include 'any': {kind_enum:?}"
        );
        assert_eq!(
            kind["default"], "any",
            "top-level kind default must be 'any'"
        );

        let entry_enum: Vec<&str> = schema["properties"]["queries"]["items"]["properties"]["kind"]
            ["enum"]
            .as_array()
            .expect("per-entry kind enum present")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            entry_enum.contains(&"any"),
            "per-entry kind enum must include 'any': {entry_enum:?}"
        );

        let compiled = jsonschema::JSONSchema::compile(schema)
            .expect("tilth_search inputSchema must be a valid JSON Schema");
        assert!(
            !compiled.is_valid(&serde_json::json!({})),
            "empty args must fail: queries is required"
        );
        assert!(
            !compiled.is_valid(&serde_json::json!({"query": "x"})),
            "the singular `query` key was dropped — only `queries` is accepted"
        );
        assert!(
            !compiled.is_valid(&serde_json::json!({"queries": [{"query": "x"}]})),
            "queries without cwd must fail: cwd is required"
        );
        assert!(compiled.is_valid(&serde_json::json!({"queries": [{"query": "x"}], "cwd": "/abs"})));
        assert!(compiled.is_valid(
            &serde_json::json!({"queries": [{"query": "x", "kind": "any"}], "cwd": "/abs"})
        ));
    }

    /// Regression for issue #47: OpenAI/Codex's strict function-schema
    /// validator rejects any tool whose `parameters` (inputSchema) is not a
    /// plain top-level object, or that uses `oneOf`/`anyOf`/`allOf`/`enum`/`not`
    /// at the top level. Anthropic/Claude tolerates the looser shape, so this
    /// only surfaced under Codex. Every advertised tool's inputSchema must
    /// satisfy the rule (nested `enum`/`allOf` under `properties` is fine —
    /// the constraint is top-level only).
    #[test]
    fn tool_schemas_are_openai_strict_compatible() {
        const FORBIDDEN_TOP_LEVEL: [&str; 5] = ["oneOf", "anyOf", "allOf", "enum", "not"];
        // edit_mode=true advertises the widest tool set (includes tilth_write).
        for tool in tool_definitions(true) {
            let name = tool["name"].as_str().expect("tool name present");
            let schema = &tool["inputSchema"];
            assert_eq!(
                schema["type"].as_str(),
                Some("object"),
                "{name}: inputSchema top level must be type 'object'"
            );
            let obj = schema.as_object().expect("inputSchema is an object");
            for key in FORBIDDEN_TOP_LEVEL {
                assert!(
                    !obj.contains_key(key),
                    "{name}: inputSchema must not use top-level '{key}' \
                     (OpenAI/Codex rejects it — see issue #47)"
                );
            }
        }
    }

    /// `tilth_files` was consolidated into `tilth_list`; it must no longer be
    /// advertised so clients can't discover a removed tool.
    #[test]
    fn tilth_files_is_not_advertised() {
        for edit_mode in [false, true] {
            let defs = tool_definitions(edit_mode);
            let names: Vec<&str> = defs.iter().filter_map(|t| t["name"].as_str()).collect();
            assert!(
                !names.contains(&"tilth_files"),
                "tilth_files must not be advertised (folded into tilth_list)"
            );
            assert!(
                names.contains(&"tilth_list"),
                "tilth_list must remain advertised"
            );
        }
    }

    /// Tool names must be unique. A duplicate function name is itself an
    /// invalid request under OpenAI/Codex. Regression for the duplicate
    /// `tilth_list` registration removed alongside #47.
    #[test]
    fn tool_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for tool in tool_definitions(true) {
            let name = tool["name"]
                .as_str()
                .expect("tool name present")
                .to_string();
            assert!(
                seen.insert(name.clone()),
                "duplicate tool definition: {name}"
            );
        }
    }

    /// Every path-taking tool must carry a required `cwd` property, and the old
    /// `root` property must be gone from every tool. All seven tools in edit mode
    /// (`tilth_diff` included) take paths and require cwd.
    #[test]
    fn every_tool_requires_cwd_and_drops_root() {
        let tools = tool_definitions(true);
        assert_eq!(tools.len(), 7, "edit mode advertises 7 path-taking tools");
        for tool in &tools {
            let name = tool["name"].as_str().expect("tool name");
            let schema = &tool["inputSchema"];
            assert!(
                schema["properties"].get("root").is_none(),
                "{name}: root property must be gone (renamed to cwd)"
            );
            assert!(
                schema["properties"].get("cwd").is_some(),
                "{name}: cwd property must be present"
            );
            let required: Vec<&str> = schema["required"]
                .as_array()
                .expect("required array present")
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            assert!(
                required.contains(&"cwd"),
                "{name}: cwd must be in required, got {required:?}"
            );
        }
    }

    #[test]
    fn cwd_property_description_tells_model_to_set_explicitly() {
        let property = cwd_property();
        let description = property["description"]
            .as_str()
            .expect("cwd property description is a string");
        assert!(
            description.contains("always set this explicitly"),
            "cwd description must tell the model to set cwd: {description}"
        );
    }

    /// `tilth_list` treats an omitted `patterns` key as a project overview,
    /// while present arrays retain glob-tree behavior and validation.
    #[test]
    fn tilth_list_schema_makes_patterns_optional_but_keeps_cwd_required() {
        let tools = tool_definitions(false);
        let list = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_list"))
            .expect("tilth_list tool definition present");
        let schema = &list["inputSchema"];

        let required: Vec<&str> = schema["required"]
            .as_array()
            .expect("required array present")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            required,
            vec!["cwd"],
            "tilth_list must require only cwd — patterns is optional"
        );
        assert!(
            schema["properties"]["patterns"]["description"]
                .as_str()
                .expect("patterns description present")
                .contains("omit patterns for a project overview"),
            "patterns description must advertise the overview omission"
        );

        let compiled = jsonschema::JSONSchema::compile(schema)
            .expect("tilth_list inputSchema must be a valid JSON Schema");
        assert!(
            compiled.is_valid(&serde_json::json!({"cwd": "/abs"})),
            "a bare cwd-only call must validate: patterns is optional"
        );
        assert!(
            !compiled.is_valid(&serde_json::json!({"patterns": ["*.rs"]})),
            "cwd stays required"
        );
        assert!(
            !compiled.is_valid(&serde_json::json!({"patterns": "*.rs", "cwd": "/abs"})),
            "non-array patterns must fail schema validation client-side"
        );
        assert!(
            !compiled.is_valid(&serde_json::json!({"patterns": [], "cwd": "/abs"})),
            "empty patterns must fail schema validation (minItems: 1)"
        );
        assert!(compiled.is_valid(&serde_json::json!({"patterns": ["*.rs"], "cwd": "/abs"})));
    }

    /// Claude Code truncates each tool `description` at 2,048 bytes. Every
    /// advertised description, in both modes, must fit under that cap or the
    /// model loses the tail of its routing guidance silently.
    #[test]
    fn tool_descriptions_fit_2kb() {
        for edit_mode in [false, true] {
            for tool in tool_definitions(edit_mode) {
                let name = tool["name"].as_str().expect("tool name present");
                let desc = tool["description"].as_str().expect("description present");
                assert!(
                    desc.len() <= 2048,
                    "{name}: description is {} bytes, over the 2048-byte MCP truncation cap",
                    desc.len()
                );
            }
        }
    }

    #[test]
    fn tilth_write_schema_includes_replace_text_branch() {
        let tools = tool_definitions(true);
        let write = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_write"))
            .expect("tilth_write tool definition present");
        let description = write["description"].as_str().expect("description");
        assert!(description.contains("replace_text uses {old,new}, must match once"));
        let branches = write["inputSchema"]["properties"]["edits"]["items"]["properties"]["ops"]
            ["items"]["oneOf"]
            .as_array()
            .expect("ops oneOf");
        let branch = branches
            .iter()
            .find(|branch| branch["properties"]["op"]["const"] == "replace_text")
            .expect("replace_text branch");
        assert_eq!(branch["required"], serde_json::json!(["op", "old", "new"]));
    }
    #[test]
    fn tilth_write_schema_replace_text_old_requires_min_length() {
        let tools = tool_definitions(true);
        let write = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_write"))
            .expect("tilth_write tool definition present");
        let branches = write["inputSchema"]["properties"]["edits"]["items"]["properties"]["ops"]
            ["items"]["oneOf"]
            .as_array()
            .expect("ops oneOf");
        let branch = branches
            .iter()
            .find(|branch| branch["properties"]["op"]["const"] == "replace_text")
            .expect("replace_text branch");
        assert_eq!(
            branch["properties"]["old"]["minLength"],
            serde_json::json!(1)
        );
    }
    #[test]
    fn tilth_write_schema_includes_create_file_branch() {
        let tools = tool_definitions(true);
        let write = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_write"))
            .expect("tilth_write tool definition present");
        let description = write["description"].as_str().expect("description");
        assert!(description.contains("create_file uses {content}"));
        let branches = write["inputSchema"]["properties"]["edits"]["items"]["properties"]["ops"]
            ["items"]["oneOf"]
            .as_array()
            .expect("ops oneOf");
        let branch = branches
            .iter()
            .find(|branch| branch["properties"]["op"]["const"] == "create_file")
            .expect("create_file branch");
        assert_eq!(branch["required"], serde_json::json!(["op", "content"]));
    }
}
