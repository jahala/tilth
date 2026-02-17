use streaming_iterator::StreamingIterator;

use crate::read::outline::code::outline_language;
use crate::types::{Lang, OutlineEntry, OutlineKind};

/// A sibling field or method resolved from the same parent struct/class/impl.
#[derive(Debug)]
pub struct ResolvedSibling {
    pub name: String,
    pub kind: OutlineKind,
    pub signature: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// Max siblings to surface in the footer.
const MAX_SIBLINGS: usize = 6;

/// Tree-sitter query for self/this field and method references by language.
/// Each pattern captures `@ref` on the accessed member name.
fn sibling_query_str(lang: Lang) -> Option<&'static str> {
    match lang {
        Lang::Rust => Some(concat!(
            "(field_expression value: (self) field: (field_identifier) @ref)\n",
            "(call_expression function: (field_expression value: (self) field: (field_identifier) @ref))\n",
        )),
        Lang::Python => Some(
            "(attribute object: (identifier) @obj attribute: (identifier) @ref)\n",
        ),
        Lang::TypeScript | Lang::JavaScript | Lang::Tsx => Some(
            "(member_expression object: (this) property: (property_identifier) @ref)\n",
        ),
        Lang::Java => Some(concat!(
            "(field_access object: (this) field: (identifier) @ref)\n",
            "(method_invocation object: (this) name: (identifier) @ref)\n",
        )),
        Lang::Go => Some(
            "(selector_expression operand: (identifier) @recv field: (field_identifier) @ref)\n",
        ),
        _ => None,
    }
}

/// Extract self/this member references from within a definition's line range.
///
/// Parses the file with tree-sitter and runs per-language queries to find
/// field accesses and method calls on `self`/`this`. Returns deduplicated,
/// sorted member names.
pub fn extract_sibling_references(content: &str, lang: Lang, def_range: (u32, u32)) -> Vec<String> {
    let Some(ts_lang) = outline_language(lang) else {
        return Vec::new();
    };

    let Some(query_str) = sibling_query_str(lang) else {
        return Vec::new();
    };

    let Ok(query) = tree_sitter::Query::new(&ts_lang, query_str) else {
        return Vec::new();
    };

    let Some(ref_idx) = query.capture_index_for_name("ref") else {
        return Vec::new();
    };

    // For Python, we also need @obj to filter `self.x` vs `other.x`.
    let obj_idx = query.capture_index_for_name("obj");
    // For Go, we need @recv to filter receiver-only accesses.
    let recv_idx = query.capture_index_for_name("recv");
    let go_receiver = if lang == Lang::Go {
        extract_go_receiver_name(content, &ts_lang)
    } else {
        None
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let bytes = content.as_bytes();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), bytes);

    let (start, end) = def_range;
    let mut names: Vec<String> = Vec::new();

    while let Some(m) = matches.next() {
        // For Python: verify @obj == "self"
        if lang == Lang::Python {
            if let Some(oi) = obj_idx {
                let obj_ok = m
                    .captures
                    .iter()
                    .any(|c| c.index == oi && c.node.utf8_text(bytes).is_ok_and(|t| t == "self"));
                if !obj_ok {
                    continue;
                }
            }
        }

        // For Go: verify @recv matches the receiver parameter name
        if lang == Lang::Go {
            if let (Some(ri), Some(ref recv_name)) = (recv_idx, &go_receiver) {
                let recv_ok = m.captures.iter().any(|c| {
                    c.index == ri
                        && c.node
                            .utf8_text(bytes)
                            .is_ok_and(|t| t == recv_name.as_str())
                });
                if !recv_ok {
                    continue;
                }
            } else if lang == Lang::Go {
                // No receiver found — can't determine self references
                continue;
            }
        }

        for cap in m.captures {
            if cap.index != ref_idx {
                continue;
            }

            let line = cap.node.start_position().row as u32 + 1;
            if line < start || line > end {
                continue;
            }

            if let Ok(text) = cap.node.utf8_text(bytes) {
                names.push(text.to_string());
            }
        }
    }

    names.sort();
    names.dedup();
    names
}

/// For Go methods, extract the receiver parameter name from the first method
/// in the file. Go receiver is the first parameter in `func (r *Type) Name()`.
fn extract_go_receiver_name(content: &str, ts_lang: &tree_sitter::Language) -> Option<String> {
    // Query for method_declaration's receiver parameter name
    let query_str = "(method_declaration receiver: (parameter_list (parameter_declaration name: (identifier) @recv)))";
    let query = tree_sitter::Query::new(ts_lang, query_str).ok()?;
    let recv_idx = query.capture_index_for_name("recv")?;

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(ts_lang).ok()?;
    let tree = parser.parse(content, None)?;

    let bytes = content.as_bytes();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), bytes);

    if let Some(m) = matches.next() {
        for cap in m.captures {
            if cap.index == recv_idx {
                return cap.node.utf8_text(bytes).ok().map(String::from);
            }
        }
    }

    None
}

/// Match extracted sibling names against a parent entry's children.
///
/// Returns up to `MAX_SIBLINGS` resolved siblings, preferring methods over fields.
pub fn resolve_siblings(
    sibling_names: &[String],
    parent_children: &[OutlineEntry],
) -> Vec<ResolvedSibling> {
    let mut resolved: Vec<ResolvedSibling> = Vec::new();

    for name in sibling_names {
        for child in parent_children {
            if child.name == *name {
                let signature = child
                    .signature
                    .clone()
                    .unwrap_or_else(|| child.name.clone());
                resolved.push(ResolvedSibling {
                    name: name.clone(),
                    kind: child.kind,
                    signature,
                    start_line: child.start_line,
                    end_line: child.end_line,
                });
                break;
            }
        }
    }

    // Sort: functions/methods first, then fields, then alphabetical within group
    resolved.sort_by(|a, b| {
        let a_is_fn = matches!(a.kind, OutlineKind::Function | OutlineKind::Method);
        let b_is_fn = matches!(b.kind, OutlineKind::Function | OutlineKind::Method);
        b_is_fn.cmp(&a_is_fn).then_with(|| a.name.cmp(&b.name))
    });

    resolved.truncate(MAX_SIBLINGS);
    resolved
}

/// Find the parent entry (struct/class/impl) whose children contain a member
/// at the given line number.
pub fn find_parent_entry(entries: &[OutlineEntry], method_line: u32) -> Option<&OutlineEntry> {
    for entry in entries {
        for child in &entry.children {
            if child.start_line == method_line {
                return Some(entry);
            }
        }
    }
    None
}
