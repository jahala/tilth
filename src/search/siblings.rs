use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use streaming_iterator::StreamingIterator;

use crate::lang::outline::outline_language;
use crate::types::{Lang, OutlineEntry, OutlineKind};

/// Global cache of compiled tree-sitter queries for sibling extraction.
/// Keyed by `(node_kind_count, field_count, query_str_ptr)` so that distinct
/// query strings for the same language (the main sibling query vs the Go
/// receiver query) are stored under separate keys. We avoid `Language::name()`
/// because ABI < 15 grammars (e.g. tree-sitter-kotlin-ng) return `None`.
#[allow(clippy::type_complexity)]
static QUERY_CACHE: LazyLock<Mutex<HashMap<(usize, usize, usize), tree_sitter::Query>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Look up or compile `query_str` for `ts_lang`, then invoke `f` with a
/// reference to the cached `Query`.  Returns `None` if compilation fails.
///
/// `query_str` must be `'static` so its pointer address is stable across
/// calls and can serve as part of the cache key.
fn with_query<R>(
    ts_lang: &tree_sitter::Language,
    query_str: &'static str,
    f: impl FnOnce(&tree_sitter::Query) -> R,
) -> Option<R> {
    use std::collections::hash_map::Entry;
    // Pointer address distinguishes different queries for the same language.
    let key = (
        ts_lang.node_kind_count(),
        ts_lang.field_count(),
        query_str.as_ptr() as usize,
    );
    let mut cache = QUERY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let query = match cache.entry(key) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => {
            let q = tree_sitter::Query::new(ts_lang, query_str).ok()?;
            e.insert(q)
        }
    };
    Some(f(query))
}

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
        Lang::Scala => Some(concat!(
            "(field_expression (identifier) @obj (identifier) @ref)\n",
            "(call_expression function: (field_expression (identifier) @obj (identifier) @ref))\n",
        )),
        Lang::Go => Some(
            "(selector_expression operand: (identifier) @recv field: (field_identifier) @ref)\n",
        ),
        Lang::CSharp => Some(concat!(
            "(member_access_expression expression: (this_expression) name: (identifier) @ref)\n",
            "(invocation_expression function: (member_access_expression expression: (this_expression) name: (identifier) @ref))\n",
        )),
        Lang::Swift => Some(
            "(navigation_expression target: (self_expression) suffix: (navigation_suffix suffix: (simple_identifier) @ref))\n",
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

    // For Go, resolve the receiver name before entering the query cache lock to
    // avoid re-entrancy on `QUERY_CACHE` (extract_go_receiver_name also uses it).
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
    let (start, end) = def_range;

    let Some(names) = with_query(&ts_lang, query_str, |query| {
        let Some(ref_idx) = query.capture_index_for_name("ref") else {
            return Vec::new();
        };

        // For Python, we also need @obj to filter `self.x` vs `other.x`.
        // For Scala, we also need @obj to filter `this.x` vs `other.x`.
        let obj_idx = query.capture_index_for_name("obj");
        // For Go, we need @recv to filter receiver-only accesses.
        let recv_idx = query.capture_index_for_name("recv");

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), bytes);
        let mut names: Vec<String> = Vec::new();

        while let Some(m) = matches.next() {
            // For Python: verify @obj == "self"
            if lang == Lang::Python {
                if let Some(oi) = obj_idx {
                    let obj_ok = m.captures.iter().any(|c| {
                        c.index == oi && c.node.utf8_text(bytes).is_ok_and(|t| t == "self")
                    });
                    if !obj_ok {
                        continue;
                    }
                }
            }

            // For Scala: verify @obj == "this"
            if lang == Lang::Scala {
                if let Some(oi) = obj_idx {
                    let obj_ok = m.captures.iter().any(|c| {
                        c.index == oi && c.node.utf8_text(bytes).is_ok_and(|t| t == "this")
                    });
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

        names
    }) else {
        return Vec::new();
    };

    let mut names = names;
    names.sort();
    names.dedup();
    names
}

/// For Go methods, extract the receiver parameter name from the first method
/// in the file. Go receiver is the first parameter in `func (r *Type) Name()`.
fn extract_go_receiver_name(content: &str, ts_lang: &tree_sitter::Language) -> Option<String> {
    // `'static` so its pointer address is a stable cache key.
    const GO_RECV_QUERY: &str = "(method_declaration receiver: (parameter_list (parameter_declaration name: (identifier) @recv)))";

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(ts_lang).ok()?;
    let tree = parser.parse(content, None)?;

    let bytes = content.as_bytes();

    // `with_query` returns `Option<Option<String>>`; flatten to `Option<String>`.
    with_query(ts_lang, GO_RECV_QUERY, |query| {
        let recv_idx = query.capture_index_for_name("recv")?;
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), bytes);

        if let Some(m) = matches.next() {
            for cap in m.captures {
                if cap.index == recv_idx {
                    return cap.node.utf8_text(bytes).ok().map(String::from);
                }
            }
        }

        None
    })
    .flatten()
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
        let a_is_fn = matches!(a.kind, OutlineKind::Function);
        let b_is_fn = matches!(b.kind, OutlineKind::Function);
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
