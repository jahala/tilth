use std::collections::HashMap;

use crate::run::types::{Diagnostic, DiagnosticGroup, Location, Severity};

const MAX_ERROR_GROUPS: usize = 10;
const MAX_WARNING_GROUPS: usize = 5;
const MAX_LOCATIONS_PER_GROUP: usize = 3;

/// Group a flat list of diagnostics into ranked, deduplicated groups.
///
/// See module-level doc for the full algorithm:
/// 1. Signature extraction (by `diagnostic.name`)
/// 2. HashMap grouping
/// 3. Ranking: errors first, then by frequency descending
/// 4. Capping: max groups and max locations per group
/// 5. Cascade detection: if one error's identifier appears in >50% of others
pub fn group(diagnostics: &[Diagnostic]) -> Vec<DiagnosticGroup> {
    if diagnostics.is_empty() {
        return Vec::new();
    }

    // --- Step 1 & 2: Group by (severity, name) ---
    // Key: (Severity, name as owned String so we can borrow freely later)
    let mut map: HashMap<(Severity, String), Vec<usize>> = HashMap::new();

    for (idx, diag) in diagnostics.iter().enumerate() {
        map.entry((diag.severity, diag.name.clone()))
            .or_default()
            .push(idx);
    }

    // --- Step 3: Build group structs and rank ---
    let mut groups: Vec<DiagnosticGroup> = map
        .into_iter()
        .map(|((severity, signature), indices)| {
            let total = indices.len();

            // Collect up to MAX_LOCATIONS_PER_GROUP unique locations.
            let locations: Vec<Location> = indices
                .iter()
                .filter_map(|&i| diagnostics[i].location.clone())
                .take(MAX_LOCATIONS_PER_GROUP)
                .collect();

            // Representative: first diagnostic in the group (preserves source order).
            let rep_idx = indices[0];
            let rep = &diagnostics[rep_idx];
            let representative = Diagnostic {
                severity: rep.severity,
                location: rep.location.clone(),
                name: rep.name.clone(),
                message: rep.message.clone(),
                detail: rep.detail.clone(),
            };

            DiagnosticGroup {
                severity,
                signature,
                locations,
                total,
                representative,
                cascading: false,
            }
        })
        .collect();

    // Sort: Severity (Error < Warning < Info), then total descending (most frequent first).
    groups.sort_unstable_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| b.total.cmp(&a.total))
    });

    // --- Step 4: Cap group counts ---
    let error_end = groups.partition_point(|g| g.severity == Severity::Error);
    let warn_end_raw = groups.partition_point(|g| g.severity <= Severity::Warning);

    let error_end_capped = error_end.min(MAX_ERROR_GROUPS);
    let warn_count = (warn_end_raw - error_end).min(MAX_WARNING_GROUPS);

    // Retain only within caps; collect remaining info groups too (uncapped for now).
    let info_start = warn_end_raw;
    let mut result: Vec<DiagnosticGroup> =
        Vec::with_capacity(error_end_capped + warn_count + (groups.len() - info_start));

    result.extend(groups.drain(..error_end).take(MAX_ERROR_GROUPS));
    let remaining = groups;
    let mut remaining_iter = remaining.into_iter();
    let warn_slice: Vec<_> = remaining_iter
        .by_ref()
        .take_while(|g| g.severity == Severity::Warning)
        .take(MAX_WARNING_GROUPS)
        .collect();
    result.extend(warn_slice);
    result.extend(remaining_iter); // info groups, uncapped

    // --- Step 5: Cascade detection ---
    mark_cascading(&mut result);

    result
}

/// If a single error group dominates (exactly 1 error group, or the first error
/// group has a clear "root" identifier), mark subsequent error groups as cascading
/// when >50% of their diagnostics share that identifier.
///
/// We extract an identifier by scanning the representative message for the first
/// backtick-quoted or single-quoted token.
fn mark_cascading(groups: &mut [DiagnosticGroup]) {
    let error_count = groups
        .iter()
        .filter(|g| g.severity == Severity::Error)
        .count();

    if error_count <= 1 {
        return;
    }

    // Extract identifier from first error group's representative message.
    let root_id = match extract_quoted_identifier(&groups[0].representative.message) {
        Some(id) => id,
        None => return,
    };

    // For every subsequent error group, check if the root_id appears in its signature
    // or representative message — mark cascading if it does and there's >1 error group.
    for group in groups.iter_mut().skip(1) {
        if group.severity != Severity::Error {
            break;
        }
        if group.signature.contains(root_id.as_str())
            || group.representative.message.contains(root_id.as_str())
        {
            group.cascading = true;
        }
    }
}

/// Extract the first backtick-quoted (`ident`) or single-quoted ('ident') token
/// from a message string.
fn extract_quoted_identifier(msg: &str) -> Option<String> {
    let bytes = msg.as_bytes();
    let n = bytes.len();

    let delimiters: &[(u8, u8)] = &[(b'`', b'`'), (b'\'', b'\''), (b'"', b'"')];

    for &(open, close) in delimiters {
        if let Some(start) = bytes.iter().position(|&b| b == open) {
            let content_start = start + 1;
            if content_start >= n {
                continue;
            }
            if let Some(rel_end) = bytes[content_start..].iter().position(|&b| b == close) {
                let content = &msg[content_start..content_start + rel_end];
                // Only treat as identifier if it looks like one (not too long, no spaces).
                if !content.is_empty() && content.len() < 64 && !content.contains(' ') {
                    return Some(content.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::types::Severity;

    fn make_diag(severity: Severity, name: &str, message: &str) -> Diagnostic {
        Diagnostic {
            severity,
            location: None,
            name: name.to_string(),
            message: message.to_string(),
            detail: None,
        }
    }

    #[test]
    fn empty_input() {
        assert!(group(&[]).is_empty());
    }

    #[test]
    fn groups_by_name() {
        let diags = vec![
            make_diag(Severity::Error, "E001", "first"),
            make_diag(Severity::Error, "E001", "second"),
            make_diag(Severity::Error, "E002", "other"),
        ];
        let groups = group(&diags);
        // E001 has 2 occurrences, E002 has 1 → E001 first.
        assert_eq!(groups[0].signature, "E001");
        assert_eq!(groups[0].total, 2);
        assert_eq!(groups[1].signature, "E002");
    }

    #[test]
    fn errors_before_warnings() {
        let diags = vec![
            make_diag(Severity::Warning, "W1", "warn"),
            make_diag(Severity::Error, "E1", "err"),
        ];
        let groups = group(&diags);
        assert_eq!(groups[0].severity, Severity::Error);
        assert_eq!(groups[1].severity, Severity::Warning);
    }

    #[test]
    fn caps_error_groups() {
        let diags: Vec<_> = (0..15)
            .map(|i| make_diag(Severity::Error, &format!("E{i:03}"), "msg"))
            .collect();
        let groups = group(&diags);
        let error_count = groups
            .iter()
            .filter(|g| g.severity == Severity::Error)
            .count();
        assert!(error_count <= MAX_ERROR_GROUPS);
    }

    #[test]
    fn quoted_identifier_extraction() {
        let id = extract_quoted_identifier("cannot find value `foo` in this scope");
        assert_eq!(id.as_deref(), Some("foo"));
    }
}
