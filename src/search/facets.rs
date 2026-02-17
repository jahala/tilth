use std::path::{Path, PathBuf};

use crate::types::Match;

/// Faceted search results grouped by match type and location.
pub struct FacetedResult {
    pub definitions: Vec<Match>,
    pub implementations: Vec<Match>,
    pub tests: Vec<Match>,
    pub usages_local: Vec<Match>,
    pub usages_cross: Vec<Match>,
}

/// Group matches into facets when there are many results (>5).
/// Partitions by definition type, test status, and package locality.
pub fn facet_matches(matches: Vec<Match>, _scope: &Path) -> FacetedResult {
    // Find primary definition's package root for local/cross determination
    let primary_pkg = matches
        .iter()
        .find(|m| m.is_definition)
        .and_then(|m| m.path.parent())
        .and_then(package_root)
        .map(std::path::Path::to_path_buf);

    let mut definitions = Vec::new();
    let mut implementations = Vec::new();
    let mut tests = Vec::new();
    let mut usages_local = Vec::new();
    let mut usages_cross = Vec::new();

    for m in matches {
        if m.is_definition && m.impl_target.is_some() {
            implementations.push(m);
        } else if m.is_definition {
            definitions.push(m);
        } else if is_test_match(&m) {
            tests.push(m);
        } else if is_same_package(&m.path, primary_pkg.as_ref()) {
            usages_local.push(m);
        } else {
            usages_cross.push(m);
        }
    }

    FacetedResult {
        definitions,
        implementations,
        tests,
        usages_local,
        usages_cross,
    }
}

/// Check if a match is in a test file or contains test markers.
fn is_test_match(m: &Match) -> bool {
    // Path-based detection
    let path_str = m.path.to_string_lossy();
    if path_str.contains("_test.")
        || path_str.contains("/test/")
        || path_str.contains("/tests/")
        || path_str.contains("_spec.")
        || path_str.contains("/spec/")
    {
        return true;
    }

    // Content-based detection
    let text = &m.text;
    text.contains("#[test]")
        || text.contains("#[cfg(test)]")
        || text.contains("@Test")
        || text.contains("def test_")
        || text.contains("it(\"")
        || text.contains("it('")
        || text.contains("describe(\"")
        || text.contains("describe('")
        || text.contains("func Test")
}

/// Check if path is in the same package as the primary definition.
fn is_same_package(path: &Path, primary_pkg: Option<&PathBuf>) -> bool {
    let Some(pkg_root) = primary_pkg else {
        return false;
    };

    path.parent()
        .and_then(package_root)
        .is_some_and(|p| p == pkg_root.as_path())
}

/// Re-export from parent module.
fn package_root(path: &Path) -> Option<&Path> {
    super::package_root(path)
}
