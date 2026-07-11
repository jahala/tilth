use streaming_iterator::StreamingIterator;

use crate::lang::outline::outline_language;
use crate::lang::treesitter::with_query;
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
    crate::lang::spec::spec(lang).sibling_query
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
    // avoid re-entrancy on `QUERY_CACHE` (the receiver extractor also uses it).
    // The extractor is supplied per-language via `spec(lang).extract_receiver`
    // (only Go has one).
    let go_receiver = crate::lang::spec::spec(lang)
        .extract_receiver
        .and_then(|extract| extract(content, &ts_lang));

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

#[cfg(test)]
mod tests {
    use super::*;

    fn child(kind: OutlineKind, name: &str, sig: Option<&str>, line: u32) -> OutlineEntry {
        OutlineEntry {
            kind,
            name: name.to_string(),
            start_line: line,
            end_line: line,
            signature: sig.map(str::to_string),
            children: Vec::new(),
            doc: None,
        }
    }

    fn parent(name: &str, children: Vec<OutlineEntry>) -> OutlineEntry {
        OutlineEntry {
            kind: OutlineKind::Struct,
            name: name.to_string(),
            start_line: 1,
            end_line: 100,
            signature: None,
            children,
            doc: None,
        }
    }

    #[test]
    fn resolve_siblings_copies_matched_child_fields() {
        let children = vec![child(
            OutlineKind::Function,
            "helper",
            Some("fn helper(&self)"),
            42,
        )];
        let resolved = resolve_siblings(&["helper".to_string()], &children);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "helper");
        assert_eq!(resolved[0].kind, OutlineKind::Function);
        assert_eq!(resolved[0].signature, "fn helper(&self)");
        assert_eq!(resolved[0].start_line, 42);
        assert_eq!(resolved[0].end_line, 42);
    }

    #[test]
    fn resolve_siblings_falls_back_to_name_when_signature_missing() {
        let children = vec![child(OutlineKind::Property, "count", None, 10)];
        let resolved = resolve_siblings(&["count".to_string()], &children);
        assert_eq!(resolved[0].signature, "count");
    }

    #[test]
    fn resolve_siblings_skips_names_with_no_matching_child() {
        let children = vec![child(OutlineKind::Function, "known", None, 5)];
        let resolved = resolve_siblings(&["known".to_string(), "unknown".to_string()], &children);
        assert_eq!(
            resolved.len(),
            1,
            "unmatched name must not appear: {resolved:?}"
        );
        assert_eq!(resolved[0].name, "known");
    }

    #[test]
    fn resolve_siblings_orders_functions_before_fields() {
        // Fields listed first in the input; functions must still sort first.
        let children = vec![
            child(OutlineKind::Property, "z_field", None, 1),
            child(OutlineKind::Function, "a_method", None, 2),
        ];
        let resolved =
            resolve_siblings(&["z_field".to_string(), "a_method".to_string()], &children);
        let names: Vec<&str> = resolved.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a_method", "z_field"]);
    }

    #[test]
    fn resolve_siblings_breaks_ties_alphabetically_within_kind() {
        let children = vec![
            child(OutlineKind::Function, "zeta", None, 1),
            child(OutlineKind::Function, "alpha", None, 2),
            child(OutlineKind::Function, "mid", None, 3),
        ];
        let resolved = resolve_siblings(
            &["zeta".to_string(), "alpha".to_string(), "mid".to_string()],
            &children,
        );
        let names: Vec<&str> = resolved.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn resolve_siblings_truncates_at_max_siblings() {
        let children: Vec<OutlineEntry> = (0..MAX_SIBLINGS + 3)
            .map(|i| child(OutlineKind::Function, &format!("m{i}"), None, i as u32))
            .collect();
        let names: Vec<String> = children.iter().map(|c| c.name.clone()).collect();
        let resolved = resolve_siblings(&names, &children);
        assert_eq!(resolved.len(), MAX_SIBLINGS);
    }

    #[test]
    fn find_parent_entry_locates_entry_owning_the_line() {
        let entries = vec![
            parent(
                "Other",
                vec![child(OutlineKind::Function, "unrelated", None, 5)],
            ),
            parent(
                "Target",
                vec![child(OutlineKind::Function, "method", None, 20)],
            ),
        ];
        let found = find_parent_entry(&entries, 20).expect("parent must be found");
        assert_eq!(found.name, "Target");
    }

    #[test]
    fn find_parent_entry_returns_none_when_no_child_matches() {
        let entries = vec![parent(
            "Solo",
            vec![child(OutlineKind::Function, "method", None, 20)],
        )];
        assert!(find_parent_entry(&entries, 999).is_none());
    }
}
