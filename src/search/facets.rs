use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::{FacetTotals, Match};

/// Faceted search results grouped by match type and location.
pub struct FacetedResult {
    pub definitions: Vec<Match>,
    pub implementations: Vec<Match>,
    pub tests: Vec<Match>,
    pub usages_local: Vec<Match>,
    pub usages_cross: Vec<Match>,
}

/// The facet a match falls in, short of the local/cross split of usages,
/// which needs the primary definition's package.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Implementation,
    Definition,
    Test,
    Usage,
}

fn kind(m: &Match) -> Kind {
    if m.is_definition && m.impl_target.is_some() {
        Kind::Implementation
    } else if m.is_definition {
        Kind::Definition
    } else if is_test_match(m) {
        Kind::Test
    } else {
        Kind::Usage
    }
}

/// The package root of the primary definition, which decides whether a usage
/// is local or cross-package.
fn primary_package(primary_def: Option<&Match>) -> Option<PathBuf> {
    primary_def
        .and_then(|m| m.path.parent())
        .and_then(tilth_core::lang::package_root)
        .map(std::path::Path::to_path_buf)
}

/// Group matches into facets when there are many results (>5).
/// Partitions by definition type, test status, and package locality.
pub fn facet_matches(matches: Vec<Match>, _scope: &Path) -> FacetedResult {
    let primary_pkg = primary_package(matches.iter().find(|m| m.is_definition));

    let mut definitions = Vec::new();
    let mut implementations = Vec::new();
    let mut tests = Vec::new();
    let mut usages_local = Vec::new();
    let mut usages_cross = Vec::new();

    for m in matches {
        match kind(&m) {
            Kind::Implementation => implementations.push(m),
            Kind::Definition => definitions.push(m),
            Kind::Test => tests.push(m),
            Kind::Usage if is_same_package(&m.path, primary_pkg.as_ref()) => usages_local.push(m),
            Kind::Usage => usages_cross.push(m),
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

/// One file's matches counted by facet. A search keeps only its best matches
/// per file, so the facet totals are summed from these counts instead.
pub struct FileFacets {
    path: PathBuf,
    implementations: usize,
    definitions: usize,
    tests: usize,
    usages: usize,
}

/// Count one file's matches by facet. Every match in `matches` is in `path`.
pub fn count_file(path: &Path, matches: &[Match]) -> FileFacets {
    let mut counts = FileFacets {
        path: path.to_path_buf(),
        implementations: 0,
        definitions: 0,
        tests: 0,
        usages: 0,
    };
    for m in matches {
        match kind(m) {
            Kind::Implementation => counts.implementations += 1,
            Kind::Definition => counts.definitions += 1,
            Kind::Test => counts.tests += 1,
            Kind::Usage => counts.usages += 1,
        }
    }
    counts
}

/// Sum per-file counts into the totals `facet_matches` would give for every
/// match at once. `primary_def` is the first definition in ranked order.
pub fn totals(files: &[FileFacets], primary_def: Option<&Match>) -> FacetTotals {
    let primary_pkg = primary_package(primary_def);
    // Files in one directory share a package root; look each directory up once.
    let mut local_dirs: HashMap<&Path, bool> = HashMap::new();
    let mut totals = FacetTotals::default();
    for f in files {
        totals.implementations += f.implementations;
        totals.definitions += f.definitions;
        totals.tests += f.tests;
        if f.usages == 0 {
            continue;
        }
        let local = match f.path.parent() {
            Some(dir) => *local_dirs
                .entry(dir)
                .or_insert_with(|| is_same_package(&f.path, primary_pkg.as_ref())),
            None => false,
        };
        if local {
            totals.usages_local += f.usages;
        } else {
            totals.usages_cross += f.usages;
        }
    }
    totals
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
        .and_then(tilth_core::lang::package_root)
        .is_some_and(|p| p == pkg_root.as_path())
}
