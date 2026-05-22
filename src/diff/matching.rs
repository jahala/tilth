use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use crate::lang::outline::outline_language;
use crate::types::{Lang, OutlineEntry, OutlineKind};

use super::{ChangeType, DiffSymbol, MatchConfidence, SymbolChange, SymbolIdentity};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build `DiffSymbol` entries from an outline and its source content.
pub(crate) fn build_diff_symbols(
    entries: &[OutlineEntry],
    content: &str,
    lang: Lang,
) -> Vec<DiffSymbol> {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();
    build_symbols_recursive(entries, &lines, lang, "", &mut out);
    out
}

/// Three-phase symbol matching: identity → structural → fuzzy.
///
/// Returns a `SymbolChange` for **every** matched pair (including unchanged)
/// plus any unmatched old (Deleted) and unmatched new (Added).
pub(crate) fn match_symbols(old: &[DiffSymbol], new: &[DiffSymbol]) -> Vec<SymbolChange> {
    let mut old_matched = vec![false; old.len()];
    let mut new_matched = vec![false; new.len()];
    let mut changes: Vec<SymbolChange> = Vec::new();

    // ------------------------------------------------------------------
    // Phase 1: Identity match
    // ------------------------------------------------------------------
    let old_by_id = index_by_identity(old);
    let new_by_id = index_by_identity(new);

    for (id, old_indices) in &old_by_id {
        if let Some(new_indices) = new_by_id.get(id) {
            // Match by position order for overloads
            for (oi, ni) in old_indices.iter().zip(new_indices.iter()) {
                let o = &old[*oi];
                let n = &new[*ni];
                old_matched[*oi] = true;
                new_matched[*ni] = true;

                if o.content_hash == n.content_hash {
                    changes.push(SymbolChange {
                        name: n.identity.name.clone(),
                        kind: n.identity.kind,
                        change: ChangeType::Unchanged,
                        match_confidence: MatchConfidence::Exact,
                        line: n.entry.start_line,
                        old_sig: None,
                        new_sig: None,
                        size_delta: Some((
                            o.entry.end_line.saturating_sub(o.entry.start_line) + 1,
                            n.entry.end_line.saturating_sub(n.entry.start_line) + 1,
                        )),
                    });
                } else if o.entry.signature != n.entry.signature {
                    changes.push(SymbolChange {
                        name: n.identity.name.clone(),
                        kind: n.identity.kind,
                        change: ChangeType::SignatureChanged,
                        match_confidence: MatchConfidence::Exact,
                        line: n.entry.start_line,
                        old_sig: o.entry.signature.clone(),
                        new_sig: n.entry.signature.clone(),
                        size_delta: Some((
                            o.entry.end_line.saturating_sub(o.entry.start_line) + 1,
                            n.entry.end_line.saturating_sub(n.entry.start_line) + 1,
                        )),
                    });
                } else {
                    changes.push(SymbolChange {
                        name: n.identity.name.clone(),
                        kind: n.identity.kind,
                        change: ChangeType::BodyChanged,
                        match_confidence: MatchConfidence::Exact,
                        line: n.entry.start_line,
                        old_sig: None,
                        new_sig: None,
                        size_delta: Some((
                            o.entry.end_line.saturating_sub(o.entry.start_line) + 1,
                            n.entry.end_line.saturating_sub(n.entry.start_line) + 1,
                        )),
                    });
                }
            }
            // Leftover old with no new counterpart — handled in phase 3 remainders
            // Leftover new with no old counterpart — handled in phase 3 remainders
        }
    }

    // ------------------------------------------------------------------
    // Phase 2: Structural hash match (unmatched only)
    // ------------------------------------------------------------------
    let mut struct_old: HashMap<(OutlineKind, u64), Vec<usize>> = HashMap::new();
    for (i, sym) in old.iter().enumerate() {
        if !old_matched[i] {
            struct_old
                .entry((sym.identity.kind, sym.structural_hash))
                .or_default()
                .push(i);
        }
    }
    let mut struct_new: HashMap<(OutlineKind, u64), Vec<usize>> = HashMap::new();
    for (i, sym) in new.iter().enumerate() {
        if !new_matched[i] {
            struct_new
                .entry((sym.identity.kind, sym.structural_hash))
                .or_default()
                .push(i);
        }
    }

    for (key, old_idxs) in &struct_old {
        if let Some(new_idxs) = struct_new.get(key) {
            if old_idxs.len() == 1 && new_idxs.len() == 1 {
                let oi = old_idxs[0];
                let ni = new_idxs[0];
                old_matched[oi] = true;
                new_matched[ni] = true;
                changes.push(SymbolChange {
                    name: new[ni].identity.name.clone(),
                    kind: new[ni].identity.kind,
                    change: ChangeType::Renamed {
                        old_name: old[oi].identity.name.clone(),
                    },
                    match_confidence: MatchConfidence::Structural,
                    line: new[ni].entry.start_line,
                    old_sig: old[oi].entry.signature.clone(),
                    new_sig: new[ni].entry.signature.clone(),
                    size_delta: Some((
                        old[oi]
                            .entry
                            .end_line
                            .saturating_sub(old[oi].entry.start_line)
                            + 1,
                        new[ni]
                            .entry
                            .end_line
                            .saturating_sub(new[ni].entry.start_line)
                            + 1,
                    )),
                });
            } else {
                // Ambiguous — mark all new entries as Added with Ambiguous confidence
                let count = (old_idxs.len() + new_idxs.len()) as u32;
                for &ni in new_idxs {
                    if !new_matched[ni] {
                        new_matched[ni] = true;
                        changes.push(SymbolChange {
                            name: new[ni].identity.name.clone(),
                            kind: new[ni].identity.kind,
                            change: ChangeType::Added,
                            match_confidence: MatchConfidence::Ambiguous(count),
                            line: new[ni].entry.start_line,
                            old_sig: None,
                            new_sig: new[ni].entry.signature.clone(),
                            size_delta: None,
                        });
                    }
                }
                for &oi in old_idxs {
                    if !old_matched[oi] {
                        old_matched[oi] = true;
                        changes.push(SymbolChange {
                            name: old[oi].identity.name.clone(),
                            kind: old[oi].identity.kind,
                            change: ChangeType::Deleted,
                            match_confidence: MatchConfidence::Ambiguous(count),
                            line: old[oi].entry.start_line,
                            old_sig: old[oi].entry.signature.clone(),
                            new_sig: None,
                            size_delta: None,
                        });
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Phase 3: Fuzzy similarity (still-unmatched)
    // ------------------------------------------------------------------
    let unmatched_old: Vec<usize> = old_matched
        .iter()
        .enumerate()
        .filter(|(_, m)| !**m)
        .map(|(i, _)| i)
        .collect();
    let unmatched_new: Vec<usize> = new_matched
        .iter()
        .enumerate()
        .filter(|(_, m)| !**m)
        .map(|(i, _)| i)
        .collect();

    // Build candidates
    let mut candidates: Vec<(usize, usize, f32)> = Vec::new();
    for &oi in &unmatched_old {
        for &ni in &unmatched_new {
            if old[oi].identity.kind != new[ni].identity.kind {
                continue;
            }
            let len_a = old[oi].source_text.len();
            let len_b = new[ni].source_text.len();
            let (min_len, max_len) = if len_a < len_b {
                (len_a, len_b)
            } else {
                (len_b, len_a)
            };
            #[allow(clippy::cast_precision_loss)]
            if max_len == 0 || (min_len as f32 / max_len as f32) < 0.8 {
                continue;
            }
            let score = strsim::sorensen_dice(&old[oi].source_text, &new[ni].source_text) as f32;
            if score >= 0.8 {
                candidates.push((oi, ni, score));
            }
        }
    }

    // Greedy: sort descending by score, match best pairs
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    let mut fuzzy_old_used = HashSet::new();
    let mut fuzzy_new_used = HashSet::new();

    for (oi, ni, score) in candidates {
        if fuzzy_old_used.contains(&oi) || fuzzy_new_used.contains(&ni) {
            continue;
        }
        fuzzy_old_used.insert(oi);
        fuzzy_new_used.insert(ni);
        old_matched[oi] = true;
        new_matched[ni] = true;

        let change = if old[oi].identity.name == new[ni].identity.name {
            ChangeType::BodyChanged
        } else {
            ChangeType::Renamed {
                old_name: old[oi].identity.name.clone(),
            }
        };

        changes.push(SymbolChange {
            name: new[ni].identity.name.clone(),
            kind: new[ni].identity.kind,
            change,
            match_confidence: MatchConfidence::Fuzzy(score),
            line: new[ni].entry.start_line,
            old_sig: old[oi].entry.signature.clone(),
            new_sig: new[ni].entry.signature.clone(),
            size_delta: Some((
                old[oi]
                    .entry
                    .end_line
                    .saturating_sub(old[oi].entry.start_line)
                    + 1,
                new[ni]
                    .entry
                    .end_line
                    .saturating_sub(new[ni].entry.start_line)
                    + 1,
            )),
        });
    }

    // ------------------------------------------------------------------
    // Remaining: unmatched old → Deleted, unmatched new → Added
    // ------------------------------------------------------------------
    for (i, matched) in old_matched.iter().enumerate() {
        if !matched {
            changes.push(SymbolChange {
                name: old[i].identity.name.clone(),
                kind: old[i].identity.kind,
                change: ChangeType::Deleted,
                match_confidence: MatchConfidence::Exact,
                line: old[i].entry.start_line,
                old_sig: old[i].entry.signature.clone(),
                new_sig: None,
                size_delta: None,
            });
        }
    }
    for (i, matched) in new_matched.iter().enumerate() {
        if !matched {
            changes.push(SymbolChange {
                name: new[i].identity.name.clone(),
                kind: new[i].identity.kind,
                change: ChangeType::Added,
                match_confidence: MatchConfidence::Exact,
                line: new[i].entry.start_line,
                old_sig: None,
                new_sig: new[i].entry.signature.clone(),
                size_delta: None,
            });
        }
    }

    changes
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn index_by_identity(symbols: &[DiffSymbol]) -> HashMap<SymbolIdentity, Vec<usize>> {
    let mut map: HashMap<SymbolIdentity, Vec<usize>> = HashMap::new();
    for (i, sym) in symbols.iter().enumerate() {
        map.entry(sym.identity.clone()).or_default().push(i);
    }
    map
}

fn build_symbols_recursive(
    entries: &[OutlineEntry],
    lines: &[&str],
    lang: Lang,
    parent_path: &str,
    out: &mut Vec<DiffSymbol>,
) {
    for entry in entries {
        let source = extract_source(lines, entry.start_line, entry.end_line);
        let content_hash = hash_string(&source);
        let structural_hash = compute_structural_hash(&source, &entry.name, lang);

        let identity = SymbolIdentity {
            kind: entry.kind,
            parent_path: parent_path.to_string(),
            name: entry.name.clone(),
        };

        out.push(DiffSymbol {
            entry: clone_entry_shallow(entry),
            identity,
            content_hash,
            structural_hash,
            source_text: source,
        });

        // Recurse into children with updated parent_path
        if !entry.children.is_empty() {
            let child_parent = if parent_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{parent_path}::{}", entry.name)
            };
            build_symbols_recursive(&entry.children, lines, lang, &child_parent, out);
        }
    }
}

fn clone_entry_shallow(entry: &OutlineEntry) -> OutlineEntry {
    OutlineEntry {
        kind: entry.kind,
        name: entry.name.clone(),
        start_line: entry.start_line,
        end_line: entry.end_line,
        signature: entry.signature.clone(),
        children: Vec::new(),
        doc: entry.doc.clone(),
    }
}

fn extract_source(lines: &[&str], start_line: u32, end_line: u32) -> String {
    let start = (start_line as usize).saturating_sub(1);
    let end = (end_line as usize).min(lines.len());
    if start >= lines.len() || start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

fn hash_string(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

fn compute_structural_hash(source: &str, symbol_name: &str, lang: Lang) -> u64 {
    let Some(ts_lang) = outline_language(lang) else {
        return hash_string(source);
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return hash_string(source);
    }

    let Some(tree) = parser.parse(source, None) else {
        return hash_string(source);
    };

    let name_range = find_name_range(source, symbol_name);
    let mut hasher = DefaultHasher::new();
    walk_ast_for_hash(
        tree.root_node(),
        source.as_bytes(),
        name_range.as_ref(),
        &mut hasher,
    );
    hasher.finish()
}

fn find_name_range(source: &str, name: &str) -> Option<Range<usize>> {
    // Find first occurrence of the symbol name as a whole word
    let bytes = source.as_bytes();
    let name_bytes = name.as_bytes();
    if name_bytes.is_empty() {
        return None;
    }
    let mut pos = 0;
    while pos + name_bytes.len() <= bytes.len() {
        if let Some(idx) = source[pos..].find(name) {
            let abs = pos + idx;
            let before_ok =
                abs == 0 || !bytes[abs - 1].is_ascii_alphanumeric() && bytes[abs - 1] != b'_';
            let after = abs + name_bytes.len();
            let after_ok = after >= bytes.len()
                || !bytes[after].is_ascii_alphanumeric() && bytes[after] != b'_';
            if before_ok && after_ok {
                return Some(abs..after);
            }
            pos = abs + 1;
        } else {
            break;
        }
    }
    // Fallback: any occurrence
    source.find(name).map(|i| i..i + name.len())
}

fn walk_ast_for_hash(
    node: tree_sitter::Node,
    source_bytes: &[u8],
    name_range: Option<&Range<usize>>,
    hasher: &mut DefaultHasher,
) {
    let kind = node.kind();

    // Skip comment nodes entirely
    if kind.contains("comment") {
        return;
    }

    // Hash the node kind
    kind.hash(hasher);

    if node.child_count() == 0 {
        // Leaf node — hash text, but exclude symbol name bytes
        let start = node.start_byte();
        let end = node.end_byte();
        let text = &source_bytes[start..end];
        let trimmed = trim_ascii(text);

        if let Some(nr) = name_range {
            // If this leaf overlaps the name range, skip its text
            if start < nr.end && end > nr.start {
                // Don't hash the text — it's the symbol name
                return;
            }
        }

        trimmed.hash(hasher);
    } else {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk_ast_for_hash(child, source_bytes, name_range, hasher);
        }
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |p| p + 1);
    &bytes[start..end]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(
        kind: OutlineKind,
        name: &str,
        start: u32,
        end: u32,
        sig: Option<&str>,
    ) -> OutlineEntry {
        OutlineEntry {
            kind,
            name: name.to_string(),
            start_line: start,
            end_line: end,
            signature: sig.map(|s| s.to_string()),
            children: Vec::new(),
            doc: None,
        }
    }

    fn make_sym(
        kind: OutlineKind,
        name: &str,
        parent: &str,
        source: &str,
        sig: Option<&str>,
    ) -> DiffSymbol {
        let content_hash = hash_string(source);
        let structural_hash = compute_structural_hash(source, name, Lang::Rust);
        DiffSymbol {
            entry: OutlineEntry {
                kind,
                name: name.to_string(),
                start_line: 1,
                end_line: 1,
                signature: sig.map(|s| s.to_string()),
                children: Vec::new(),
                doc: None,
            },
            identity: SymbolIdentity {
                kind,
                parent_path: parent.to_string(),
                name: name.to_string(),
            },
            content_hash,
            structural_hash,
            source_text: source.to_string(),
        }
    }

    // 1. identity_match_body_changed
    #[test]
    fn identity_match_body_changed() {
        let old = vec![make_sym(
            OutlineKind::Function,
            "foo",
            "",
            "fn foo() { 1 }",
            Some("fn foo()"),
        )];
        let new = vec![make_sym(
            OutlineKind::Function,
            "foo",
            "",
            "fn foo() { 2 }",
            Some("fn foo()"),
        )];
        let changes = match_symbols(&old, &new);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0].change, ChangeType::BodyChanged));
        assert!(matches!(
            changes[0].match_confidence,
            MatchConfidence::Exact
        ));
    }

    // 2. identity_match_signature_changed
    #[test]
    fn identity_match_signature_changed() {
        let old = vec![make_sym(
            OutlineKind::Function,
            "foo",
            "",
            "fn foo(x: i32) { x }",
            Some("fn foo(x: i32)"),
        )];
        let new = vec![make_sym(
            OutlineKind::Function,
            "foo",
            "",
            "fn foo(x: i32, y: i32) { x + y }",
            Some("fn foo(x: i32, y: i32)"),
        )];
        let changes = match_symbols(&old, &new);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0].change, ChangeType::SignatureChanged));
        assert_eq!(changes[0].old_sig.as_deref(), Some("fn foo(x: i32)"));
        assert_eq!(
            changes[0].new_sig.as_deref(),
            Some("fn foo(x: i32, y: i32)")
        );
    }

    // 3. unchanged_symbol
    #[test]
    fn unchanged_symbol() {
        let src = "fn foo() { 42 }";
        let old = vec![make_sym(
            OutlineKind::Function,
            "foo",
            "",
            src,
            Some("fn foo()"),
        )];
        let new = vec![make_sym(
            OutlineKind::Function,
            "foo",
            "",
            src,
            Some("fn foo()"),
        )];
        let changes = match_symbols(&old, &new);
        assert_eq!(changes.len(), 1, "Unchanged symbols must be in the output");
        assert!(matches!(changes[0].change, ChangeType::Unchanged));
    }

    // 4. overloaded_methods
    #[test]
    fn overloaded_methods() {
        let old = vec![
            make_sym(
                OutlineKind::Function,
                "process",
                "",
                "fn process() { a }",
                Some("fn process()"),
            ),
            make_sym(
                OutlineKind::Function,
                "process",
                "",
                "fn process() { b }",
                Some("fn process()"),
            ),
        ];
        let new = vec![
            make_sym(
                OutlineKind::Function,
                "process",
                "",
                "fn process() { a_new }",
                Some("fn process()"),
            ),
            make_sym(
                OutlineKind::Function,
                "process",
                "",
                "fn process() { b_new }",
                Some("fn process()"),
            ),
        ];
        let changes = match_symbols(&old, &new);
        // Both should be matched (by position order) — both changed
        let body_changes: Vec<_> = changes
            .iter()
            .filter(|c| matches!(c.change, ChangeType::BodyChanged))
            .collect();
        assert_eq!(body_changes.len(), 2);
    }

    // 5. rename_detection_structural_hash
    #[test]
    fn rename_detection_structural_hash() {
        let old = vec![make_sym(
            OutlineKind::Function,
            "foo",
            "",
            "fn foo(x: i32) -> i32 { x + 1 }",
            Some("fn foo(x: i32) -> i32"),
        )];
        let new = vec![make_sym(
            OutlineKind::Function,
            "bar",
            "",
            "fn bar(x: i32) -> i32 { x + 1 }",
            Some("fn bar(x: i32) -> i32"),
        )];
        let changes = match_symbols(&old, &new);
        assert_eq!(changes.len(), 1);
        assert!(
            matches!(&changes[0].change, ChangeType::Renamed { old_name } if old_name == "foo")
        );
        assert!(matches!(
            changes[0].match_confidence,
            MatchConfidence::Structural
        ));
    }

    // 6. structural_hash_excludes_name
    #[test]
    fn structural_hash_excludes_name() {
        let hash_a = compute_structural_hash("fn foo(x: i32) -> i32 { x + 1 }", "foo", Lang::Rust);
        let hash_b = compute_structural_hash("fn bar(x: i32) -> i32 { x + 1 }", "bar", Lang::Rust);
        assert_eq!(hash_a, hash_b, "structural hash should exclude symbol name");
    }

    // 7. structural_hash_differs_for_bodies
    #[test]
    fn structural_hash_differs_for_bodies() {
        let hash_a = compute_structural_hash("fn foo() { 1 + 2 }", "foo", Lang::Rust);
        let hash_b = compute_structural_hash("fn foo() { 3 * 4 }", "foo", Lang::Rust);
        assert_ne!(
            hash_a, hash_b,
            "different bodies should produce different hashes"
        );
    }

    // 8. structural_hash_skips_comments
    #[test]
    fn structural_hash_skips_comments() {
        let hash_a = compute_structural_hash("fn foo() { x + 1 }", "foo", Lang::Rust);
        let hash_b = compute_structural_hash(
            "// this is a comment\nfn foo() { x + 1 }",
            "foo",
            Lang::Rust,
        );
        assert_eq!(hash_a, hash_b, "comments should not affect structural hash");
    }

    // 9. ambiguity_multiple_structural
    #[test]
    fn ambiguity_multiple_structural() {
        // Two old and two new with same structural hash
        let old = vec![
            make_sym(
                OutlineKind::Function,
                "alpha",
                "",
                "fn alpha(x: i32) -> i32 { x + 1 }",
                None,
            ),
            make_sym(
                OutlineKind::Function,
                "beta",
                "",
                "fn beta(x: i32) -> i32 { x + 1 }",
                None,
            ),
        ];
        let new = vec![
            make_sym(
                OutlineKind::Function,
                "gamma",
                "",
                "fn gamma(x: i32) -> i32 { x + 1 }",
                None,
            ),
            make_sym(
                OutlineKind::Function,
                "delta",
                "",
                "fn delta(x: i32) -> i32 { x + 1 }",
                None,
            ),
        ];
        let changes = match_symbols(&old, &new);
        let ambiguous: Vec<_> = changes
            .iter()
            .filter(|c| matches!(c.match_confidence, MatchConfidence::Ambiguous(_)))
            .collect();
        assert!(
            ambiguous.len() >= 2,
            "multiple structural matches should produce Ambiguous entries"
        );
    }

    // 10. fuzzy_match
    #[test]
    fn fuzzy_match() {
        // >80% Jaccard similar, different names, same kind, no identity match.
        // Bodies must differ structurally (different AST) so Phase 2 doesn't catch them,
        // but share enough tokens for Jaccard ≥0.8.
        let old = vec![make_sym(
            OutlineKind::Function,
            "handle_request",
            "",
            "fn handle_request(a: i32, b: i32, c: i32) { let x = a + b + c; println!(x); x }",
            None,
        )];
        let new = vec![make_sym(
            OutlineKind::Function,
            "process_request",
            "",
            "fn process_request(a: i32, b: i32, c: i32) { let x = a + b + c; println!(x); x + 0 }",
            None,
        )];
        let changes = match_symbols(&old, &new);
        let fuzzy: Vec<_> = changes
            .iter()
            .filter(|c| matches!(c.match_confidence, MatchConfidence::Fuzzy(_)))
            .collect();
        assert_eq!(fuzzy.len(), 1);
        assert!(matches!(
            &fuzzy[0].change,
            ChangeType::Renamed { old_name } if old_name == "handle_request"
        ));
    }

    // 11. below_fuzzy_threshold
    #[test]
    fn below_fuzzy_threshold() {
        let old = vec![make_sym(
            OutlineKind::Function,
            "aaa",
            "",
            "fn aaa() { completely different code here with unique tokens alpha beta gamma }",
            None,
        )];
        let new = vec![make_sym(
            OutlineKind::Function,
            "bbb",
            "",
            "fn bbb() { nothing similar at all with other words delta epsilon zeta }",
            None,
        )];
        let changes = match_symbols(&old, &new);
        let added = changes
            .iter()
            .any(|c| matches!(c.change, ChangeType::Added));
        let deleted = changes
            .iter()
            .any(|c| matches!(c.change, ChangeType::Deleted));
        assert!(added, "unmatched new should be Added");
        assert!(deleted, "unmatched old should be Deleted");
    }

    // 12. prefilter_token_ratio
    #[test]
    fn prefilter_token_ratio() {
        let old = vec![make_sym(
            OutlineKind::Function,
            "tiny",
            "",
            "fn tiny() { x }",
            None,
        )];
        let new = vec![make_sym(
            OutlineKind::Function,
            "huge",
            "",
            "fn huge() { a b c d e f g h i j k l m n o p q r s t u v w x y z aa bb cc dd ee ff gg hh ii jj kk ll mm nn oo pp qq rr ss tt uu vv ww xx }",
            None,
        )];
        let changes = match_symbols(&old, &new);
        // Should not fuzzy-match due to size ratio pre-filter
        let fuzzy: Vec<_> = changes
            .iter()
            .filter(|c| matches!(c.match_confidence, MatchConfidence::Fuzzy(_)))
            .collect();
        assert!(
            fuzzy.is_empty(),
            "vastly different sizes should be skipped by pre-filter"
        );
    }

    // 13. added_and_deleted
    #[test]
    fn added_and_deleted() {
        let old = vec![make_sym(
            OutlineKind::Function,
            "old_fn",
            "",
            "fn old_fn() { removed }",
            None,
        )];
        let new = vec![make_sym(
            OutlineKind::Function,
            "new_fn",
            "",
            "fn new_fn() { completely brand new unique code that is nothing like the old }",
            None,
        )];
        let changes = match_symbols(&old, &new);
        assert!(changes
            .iter()
            .any(|c| matches!(c.change, ChangeType::Deleted) && c.name == "old_fn"));
        assert!(changes
            .iter()
            .any(|c| matches!(c.change, ChangeType::Added) && c.name == "new_fn"));
    }

    // 14. empty_inputs
    #[test]
    fn empty_inputs() {
        assert!(match_symbols(&[], &[]).is_empty());
        // One side empty
        let sym = vec![make_sym(OutlineKind::Function, "f", "", "fn f() {}", None)];
        let del = match_symbols(&sym, &[]);
        assert_eq!(del.len(), 1);
        assert!(matches!(del[0].change, ChangeType::Deleted));
        let add = match_symbols(&[], &sym);
        assert_eq!(add.len(), 1);
        assert!(matches!(add[0].change, ChangeType::Added));
    }

    // 15. impl_block_parent_path
    #[test]
    fn impl_block_parent_path() {
        let src = "impl Foo {\n    fn bar(&self) { 1 }\n}";
        let entries = crate::lang::outline::get_outline_entries(src, Lang::Rust);
        let symbols = build_diff_symbols(&entries, src, Lang::Rust);
        let bar = symbols
            .iter()
            .find(|s| s.identity.name == "bar")
            .expect("should find bar method");
        assert!(
            bar.identity.parent_path.contains("impl Foo"),
            "parent_path should contain 'impl Foo', got: {}",
            bar.identity.parent_path
        );
    }

    // 16. build_diff_symbols_rust
    #[test]
    fn build_diff_symbols_rust() {
        let src = "fn hello() -> i32 { 42 }\nstruct Point { x: f64, y: f64 }";
        let entries = crate::lang::outline::get_outline_entries(src, Lang::Rust);
        let symbols = build_diff_symbols(&entries, src, Lang::Rust);
        assert!(symbols.len() >= 2);
        let hello = symbols.iter().find(|s| s.identity.name == "hello");
        assert!(hello.is_some());
        let point = symbols.iter().find(|s| s.identity.name == "Point");
        assert!(point.is_some());
    }

    // 17. build_diff_symbols_impl_children
    #[test]
    fn build_diff_symbols_impl_children() {
        let src = "impl MyStruct {\n    fn method_a(&self) { }\n    fn method_b(&self) { }\n}";
        let entries = crate::lang::outline::get_outline_entries(src, Lang::Rust);
        let symbols = build_diff_symbols(&entries, src, Lang::Rust);
        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.identity.name == "method_a" || s.identity.name == "method_b")
            .collect();
        assert_eq!(methods.len(), 2);
        for m in &methods {
            assert!(
                m.identity.parent_path.contains("impl MyStruct"),
                "method {} should have parent_path containing 'impl MyStruct', got: {}",
                m.identity.name,
                m.identity.parent_path
            );
        }
    }
}
