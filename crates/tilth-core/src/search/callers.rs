//! Structural call-site detection: every place a target symbol is called,
//! found with one parallel walk, a bloom and byte pre-filter, and a
//! tree-sitter pass over the files that survive.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use streaming_iterator::StreamingIterator;

use crate::error::TilthError;
use crate::lang::outline::outline_language;
use crate::lang::treesitter::is_bodiless_declaration;
use crate::lang::{detect_file_type, detect_file_type_with_content};
use crate::types::FileType;

/// A single caller match — a call site of a target symbol.
#[derive(Debug)]
pub struct CallerMatch {
    pub path: PathBuf,
    pub line: u32,
    pub calling_function: String,
    pub call_text: String,
    /// Line range of the calling function (for expand).
    pub caller_range: Option<(u32, u32)>,
    /// The call sits in test code that only the source says is test code: a
    /// Rust `#[test]` function, a `#[cfg(test)]` module, or a file under a
    /// crate's `tests/` directory. `is_test_file` sees only path conventions.
    pub in_test: bool,
    /// File content, already read during `find_callers_batch` — avoids re-reading during expand.
    /// Shared across all call sites in the same file via reference counting.
    pub content: Arc<String>,
}

/// What a caller walk returns: one `per_file` result for each file with a
/// call site, in path order, and the code files it skipped for size although
/// they contain a target, sorted.
#[derive(Debug)]
pub struct CallerWalk<T> {
    pub per_file: Vec<T>,
    pub skipped_large: Vec<PathBuf>,
    /// The walk found at least `stop_after` call sites. It may have stopped
    /// before visiting every file, so counts drawn from `per_file` are lower
    /// bounds. Always false without `stop_after`.
    pub reached_bound: bool,
}

/// Find every call site of any symbol in `targets` under `scope`, in one
/// parallel walk that runs to the end unless `stop_after` is set.
///
/// `per_file` receives one file's call sites as (`target_name`, match)
/// pairs in line order, and reduces them to what the caller keeps. It runs
/// once per file that holds at least one call site. Its results come back
/// sorted by file path, so the output does not depend on thread timing. A
/// caller that needs exact totals counts inside `per_file` and keeps only
/// the sites it will show; that is what bounds memory on a large tree.
/// Code files over `MAX_FILE_SIZE` are not parsed; the ones whose text holds
/// a target are listed in `skipped_large`, so a caller can say so.
///
/// `stop_after` is for callers that must stay fast on any tree and can say
/// their count is a lower bound: the walk stops once it has found that many
/// call sites, and `reached_bound` tells the caller so. Which files it
/// reached first then depends on thread timing.
// The std hasher is part of the signature dependents pin; a generic hasher
// would change it for every caller.
#[allow(clippy::implicit_hasher)]
pub fn find_callers_batch<T, F>(
    targets: &HashSet<String>,
    scope: &Path,
    bloom: &crate::index::bloom::BloomFilterCache,
    glob: Option<&str>,
    stop_after: Option<usize>,
    per_file: F,
) -> Result<CallerWalk<T>, TilthError>
where
    T: Send,
    F: Fn(Vec<(String, CallerMatch)>) -> T + Sync,
{
    let found: Mutex<Vec<(PathBuf, T)>> = Mutex::new(Vec::new());
    let skipped_large: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    let sites_found = AtomicUsize::new(0);
    let reached = |n: usize| stop_after.is_some_and(|bound| n >= bound);

    let walker = super::walk::walker(scope, glob)?;

    walker.run(|| {
        let found = &found;
        let skipped_large = &skipped_large;
        let sites_found = &sites_found;
        let per_file = &per_file;

        Box::new(move |entry| {
            if reached(sites_found.load(Ordering::Relaxed)) {
                return ignore::WalkState::Quit;
            }

            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            if oversized_code_holding(path, targets) {
                skipped_large
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(path.to_path_buf());
                return ignore::WalkState::Continue;
            }

            // Read + size-gate + bloom prefilter in one shared step.
            let Some((content, _mtime)) = super::bloom_walk::read_with_bloom_check(
                path,
                targets,
                bloom,
                crate::source::MAX_FILE_SIZE,
            ) else {
                return ignore::WalkState::Continue;
            };

            // Fast byte check via memchr::memmem (SIMD) — cheap second pass that
            // eliminates bloom false positives before tree-sitter parses.
            if !targets
                .iter()
                .any(|t| memchr::memmem::find(content.as_bytes(), t.as_bytes()).is_some())
            {
                return ignore::WalkState::Continue;
            }

            // Only process files with tree-sitter grammars
            let file_type = detect_file_type_with_content(path, &content);
            let FileType::Code(lang) = file_type else {
                return ignore::WalkState::Continue;
            };

            let Some(ts_lang) = outline_language(lang) else {
                return ignore::WalkState::Continue;
            };

            let mut file_callers =
                find_callers_treesitter_batch(path, scope, targets, &ts_lang, &content, lang);

            if !file_callers.is_empty() {
                // Stable, so two calls on one line keep the query's order.
                file_callers.sort_by_key(|(_, m)| m.line);
                sites_found.fetch_add(file_callers.len(), Ordering::Relaxed);
                let kept = per_file(file_callers);
                found
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((path.to_path_buf(), kept));
            }

            ignore::WalkState::Continue
        })
    });

    let mut found = found
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    found.sort_by(|(a, _), (b, _)| a.cmp(b));
    let mut skipped_large = skipped_large
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    skipped_large.sort();
    Ok(CallerWalk {
        per_file: found.into_iter().map(|(_, kept)| kept).collect(),
        skipped_large,
        reached_bound: reached(sites_found.into_inner()),
    })
}

/// The types that define each of `names` under `scope`: for each name, the
/// distinct owners (see [`super::scope::definition_owner`]) of its
/// definitions, the same owners caller labels carry. A definition with no
/// owner, such as a plain function, adds none, and neither does a member
/// declared without a body (a trait or interface method): a call through
/// it runs an implementor's body, whose type is counted. Names no type
/// defines are left out.
///
/// A call site names a method without its type (`x.run()`), so a name that
/// several types define cannot be traced to one of them by name alone.
/// Files over the size cap are not parsed, as in [`find_callers_batch`].
// The std hasher is part of the signature, as in `find_callers_batch`.
#[allow(clippy::implicit_hasher)]
pub fn definition_owners(
    names: &HashSet<String>,
    scope: &Path,
    bloom: &crate::index::bloom::BloomFilterCache,
    glob: Option<&str>,
) -> Result<HashMap<String, BTreeSet<String>>, TilthError> {
    let found: Mutex<HashMap<String, BTreeSet<String>>> = Mutex::new(HashMap::new());
    if names.is_empty() {
        return Ok(HashMap::new());
    }
    let walker = super::walk::walker(scope, glob)?;

    walker.run(|| {
        let found = &found;
        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }
            let path = entry.path();
            let Some((content, _mtime)) = super::bloom_walk::read_with_bloom_check(
                path,
                names,
                bloom,
                crate::source::MAX_FILE_SIZE,
            ) else {
                return ignore::WalkState::Continue;
            };
            if !names
                .iter()
                .any(|n| memchr::memmem::find(content.as_bytes(), n.as_bytes()).is_some())
            {
                return ignore::WalkState::Continue;
            }
            let FileType::Code(lang) = detect_file_type_with_content(path, &content) else {
                return ignore::WalkState::Continue;
            };
            let Some(ts_lang) = outline_language(lang) else {
                return ignore::WalkState::Continue;
            };
            let owners = owners_in_file(&content, names, &ts_lang, lang);
            if !owners.is_empty() {
                let mut found = found
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for (name, owner) in owners {
                    found.entry(name).or_default().insert(owner);
                }
            }
            ignore::WalkState::Continue
        })
    });

    Ok(found
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// The (name, owner) pair of each owned definition of one of `names` in a
/// file's content.
fn owners_in_file(
    content: &str,
    names: &HashSet<String>,
    ts_lang: &tree_sitter::Language,
    lang: crate::types::Lang,
) -> Vec<(String, String)> {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(ts_lang).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    let mut owners = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if let Some(name) = super::scope::definition_name(node, &lines, lang) {
            if names.contains(&name) && !is_bodiless_declaration(node) {
                if let Some(owner) = super::scope::definition_owner(node, &lines, lang) {
                    owners.push((name, owner));
                }
            }
        }
        if cursor.goto_first_child() || cursor.goto_next_sibling() {
            continue;
        }
        let mut done = true;
        while cursor.goto_parent() {
            if cursor.goto_next_sibling() {
                done = false;
                break;
            }
        }
        if done {
            return owners;
        }
    }
}

/// Is `path` a code file the walk would parse, over the size cap, whose text
/// names one of `targets` as a whole word? `renew` is no call of `new`.
fn oversized_code_holding(path: &Path, targets: &HashSet<String>) -> bool {
    let too_big = std::fs::metadata(path).is_ok_and(|m| m.len() > crate::source::MAX_FILE_SIZE);
    if !too_big {
        return false;
    }
    let FileType::Code(lang) = detect_file_type(path) else {
        return false;
    };
    if outline_language(lang).is_none() {
        return false;
    }
    std::fs::read(path).is_ok_and(|bytes| targets.iter().any(|t| super::deps::mentions(&bytes, t)))
}

/// Tree-sitter call site detection for a set of target symbols.
/// Returns tuples of (`matched_target_name`, `CallerMatch`).
fn find_callers_treesitter_batch(
    path: &Path,
    scope: &Path,
    targets: &HashSet<String>,
    ts_lang: &tree_sitter::Language,
    content: &str,
    lang: crate::types::Lang,
) -> Vec<(String, CallerMatch)> {
    // Get the query string for this language
    let Some(query_str) = super::callee_query::callee_query_str(lang) else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(ts_lang).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let content_bytes = content.as_bytes();
    let lines: Vec<&str> = content.lines().collect();

    // One Arc per file — all call sites share the same allocation.
    let shared_content: Arc<String> = Arc::new(content.to_string());

    let callee_filter = crate::lang::spec::spec(lang).callee_filter;
    // Test code the path alone does not reveal (see `test_site`).
    let test_site = crate::lang::spec::spec(lang).test_site;

    let Some(callers) = super::callee_query::with_callee_query(ts_lang, query_str, |query| {
        let Some(callee_idx) = query.capture_index_for_name("callee") else {
            return Vec::new();
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), content_bytes);
        let mut callers = Vec::new();

        while let Some(m) = matches.next() {
            for cap in m.captures() {
                if cap.index != callee_idx {
                    continue;
                }

                // Check if the captured text matches any of our target symbols
                let Ok(text) = cap.node.utf8_text(content_bytes) else {
                    continue;
                };

                if !targets.contains(text) {
                    continue;
                }

                // Some patterns match more than a call (see `callee_filter`).
                if callee_filter.is_some_and(|keep| !keep(&cap.node)) {
                    continue;
                }

                let matched_target = text.to_string();

                // Found a call site! Now walk up to find the calling function
                let line = cap.node.start_position().row as u32 + 1;

                // Get the call text (the whole call expression, not just the
                // callee). When the parent spans lines no single line is the
                // call, so show the callee's own line instead of the bare name.
                // Applies to every language; a macro's token tree makes it the
                // common case for Rust.
                let call_node = cap.node.parent().unwrap_or(cap.node);
                let same_line = call_node.start_position().row == call_node.end_position().row;
                let row = if same_line {
                    call_node.start_position().row
                } else {
                    cap.node.start_position().row
                };
                let call_text: String = lines
                    .get(row)
                    .map_or_else(|| matched_target.clone(), |l| l.trim().to_string());

                // Walk up the tree to find the enclosing function
                let (calling_function, caller_range) =
                    find_enclosing_function(cap.node, &lines, lang);

                callers.push((
                    matched_target,
                    CallerMatch {
                        path: path.to_path_buf(),
                        line,
                        calling_function,
                        call_text,
                        caller_range,
                        in_test: test_site
                            .is_some_and(|in_test| in_test(&cap.node, content_bytes, path, scope)),
                        content: Arc::clone(&shared_content),
                    },
                ));
            }
        }

        callers
    }) else {
        return Vec::new();
    };

    callers
}

/// Walk up the AST from a node to find the enclosing function definition.
/// Returns (`function_name`, `line_range`). Top-level renders as `"<top-level>"`.
fn find_enclosing_function(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: crate::types::Lang,
) -> (String, Option<(u32, u32)>) {
    match super::scope::walk_to_enclosing_definition(node, lines, lang) {
        Some((_, name, range)) => (name, Some(range)),
        None => ("<top-level>".to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each name maps to the types whose definitions of it a caller label
    /// would name. A plain function of the same name adds no type, and
    /// neither does a trait method declared without a body.
    #[test]
    fn definition_owners_lists_the_types_that_define_a_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("m.rs"),
            "struct A;\nstruct B;\nimpl A { fn new() -> A { A } }\n\
             impl B { fn new() -> B { B } fn only() {} }\nfn new() {}\nfn free() {}\n\
             trait Run { fn only(&self); }\n",
        )
        .unwrap();
        let bloom = crate::index::bloom::BloomFilterCache::new();
        let names: HashSet<String> = ["new", "only", "free"].map(String::from).into();
        let owners = definition_owners(&names, dir.path(), &bloom, None).unwrap();
        let of = |n: &str| owners.get(n).map(|o| o.iter().cloned().collect::<Vec<_>>());
        assert_eq!(of("new"), Some(vec!["A".to_string(), "B".to_string()]));
        assert_eq!(of("only"), Some(vec!["B".to_string()]));
        assert_eq!(of("free"), None);
    }

    /// The bloom pre-filter must never hide a file that holds a call site. A
    /// quote inside a triple-quoted string, or an apostrophe in a `#` comment,
    /// once made the scanner read the rest of the file as a string, so the
    /// names after it never reached the filter and their call sites vanished.
    #[test]
    fn call_sites_after_a_triple_quoted_string_or_hash_comment_are_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.py"), "def helper_after():\n    pass\n").unwrap();
        std::fs::write(
            dir.path().join("uses_docstring.py"),
            "FIXTURE = \"\"\"\n{\"prompt\": \"count ripgrep's lines\"}\n\"\"\"\n\n\
             def docstring_caller():\n    helper_after()\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("uses_comment.py"),
            "# don't call this twice\ndef comment_caller():\n    helper_after()\n",
        )
        .unwrap();

        let bloom = crate::index::bloom::BloomFilterCache::new();
        let targets: HashSet<String> = std::iter::once("helper_after".to_string()).collect();
        let found = find_callers_batch(&targets, dir.path(), &bloom, None, None, |sites| {
            sites
                .into_iter()
                .map(|(_, m)| m.calling_function)
                .collect::<Vec<_>>()
        })
        .unwrap();
        let callers: Vec<&str> = found
            .per_file
            .iter()
            .flatten()
            .map(String::as_str)
            .collect();
        assert_eq!(callers, ["comment_caller", "docstring_caller"]);
    }

    /// Each `(line, label)` of the call sites of `target` in `file`.
    fn caller_labels(file: &str, source: &str, target: &str) -> Vec<(u32, String)> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(file), source).unwrap();
        let bloom = crate::index::bloom::BloomFilterCache::new();
        let targets: HashSet<String> = std::iter::once(target.to_string()).collect();
        let found = find_callers_batch(&targets, dir.path(), &bloom, None, None, |sites| {
            sites
                .into_iter()
                .map(|(_, m)| (m.line, m.calling_function))
                .collect::<Vec<_>>()
        })
        .unwrap();
        let mut labels: Vec<(u32, String)> = found.per_file.into_iter().flatten().collect();
        labels.sort();
        labels
    }

    fn labels(pairs: &[(u32, &str)]) -> Vec<(u32, String)> {
        pairs.iter().map(|&(l, s)| (l, s.to_string())).collect()
    }

    /// A call in a local variable's initializer is made by the function that
    /// declares the variable, so it carries that function's name. A
    /// top-level binding has no function around it and keeps its own name.
    #[test]
    fn a_call_in_a_local_initializer_is_labelled_by_its_function() {
        let ts = "function target() { return 1; }\n\
                  function outer() {\n\
                  \x20 const result = target();\n\
                  \x20 let other = target(), more = 2;\n\
                  \x20 var legacy = target();\n\
                  \x20 function nested() { const result = target(); return result; }\n\
                  }\n\
                  class Service {\n\
                  \x20 run() { const result = target(); return result; }\n\
                  }\n\
                  function* generate() { const value = target(); yield value; }\n\
                  const topLevel = target();\n";
        assert_eq!(
            caller_labels("calls.ts", ts, "target"),
            labels(&[
                (3, "outer"),
                (4, "outer"),
                (5, "outer"),
                (6, "nested"),
                (9, "Service.run"),
                (11, "generate"),
                (12, "topLevel"),
            ])
        );

        let rust = "fn rust_target() -> u32 { 1 }\n\
                    fn rust_outer() -> u32 {\n\
                    \x20   const LOCAL: u32 = rust_target();\n\
                    \x20   static S: u32 = rust_target();\n\
                    \x20   LOCAL + S\n\
                    }\n\
                    const TOP: u32 = rust_target();\n";
        assert_eq!(
            caller_labels("lib.rs", rust, "rust_target"),
            labels(&[(3, "rust_outer"), (4, "rust_outer"), (7, "TOP")])
        );

        // A function value is labelled by its own name or the name it is
        // bound to; an anonymous callback belongs to what is around it.
        let js = "res.send = function send(body) {\n\
                    var file = target(body);\n\
                  };\n\
                  app.handle = function (req) {\n\
                    const out = target(req);\n\
                  };\n\
                  function outer() {\n\
                    const cb = () => target();\n\
                    [1].map(function () { const v = target(); });\n\
                  }\n\
                  const api = { load: function () { const v = target(); } };\n\
                  describe('x', () => { const app = target(); });\n";
        assert_eq!(
            caller_labels("lib.js", js, "target"),
            labels(&[
                (2, "send"),
                (5, "handle"),
                (8, "cb"),
                (9, "outer"),
                (11, "load"),
                (12, "<top-level>"),
            ])
        );

        let bash = "target() { echo hi; }\nouter() {\n  value=$(target)\n  echo \"$value\"\n}\n";
        assert_eq!(
            caller_labels("run.sh", bash, "target"),
            labels(&[(3, "outer")])
        );
    }

    /// A method's label names the type it belongs to, however the language
    /// attaches it: a Rust `impl` block, a Go receiver, or a C++ definition
    /// outside its class.
    #[test]
    fn a_method_label_names_its_type() {
        let rust = "struct A;\nimpl A {\n    fn run(&self) { go(); }\n}\n\
                    impl<T> Wrap<T> {\n    fn run(&self) { go(); }\n}\n";
        assert_eq!(
            caller_labels("m.rs", rust, "go"),
            labels(&[(3, "A.run"), (6, "Wrap.run")])
        );

        let go = "package p\n\ntype Context struct{}\n\n\
                  func (c *Context) reset() { work() }\n\n\
                  func (r JSON) Render() error { work(); return nil }\n";
        assert_eq!(
            caller_labels("ctx.go", go, "work"),
            labels(&[(5, "Context.reset"), (7, "JSON.Render")])
        );

        let cpp = "struct B { int run(); };\nint B::run() { return go(); }\n";
        assert_eq!(caller_labels("b.cpp", cpp, "go"), labels(&[(2, "B.run")]));
    }

    /// A file skipped for size is listed only when it names a target as a
    /// whole word: `renew` holds the text `new`, but no call of `new`.
    #[test]
    fn a_big_file_is_listed_only_when_it_names_a_target_as_a_word() {
        let dir = tempfile::tempdir().unwrap();
        let limit = usize::try_from(crate::source::MAX_FILE_SIZE).unwrap();
        let pad = "// padding line to push the file over the size limit\n".repeat(limit / 50 + 1);
        std::fs::write(
            dir.path().join("word.rs"),
            format!("{pad}fn a() {{ new(); }}\n"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("part.rs"),
            format!("{pad}fn b() {{ renew(); }}\n"),
        )
        .unwrap();
        let bloom = crate::index::bloom::BloomFilterCache::new();
        let targets: HashSet<String> = std::iter::once("new".to_string()).collect();
        let walk =
            find_callers_batch(&targets, dir.path(), &bloom, None, None, |s| s.len()).unwrap();
        let names: Vec<_> = walk
            .skipped_large
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["word.rs"]);
    }

    /// `reached_bound` is set exactly when the walk found `stop_after` sites,
    /// so a caller labels a count a lower bound only when it may be one.
    #[test]
    fn reached_bound_says_whether_the_walk_found_stop_after_sites() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn a() {\n    go();\n    go();\n}\n",
        )
        .unwrap();
        let bloom = crate::index::bloom::BloomFilterCache::new();
        let targets: HashSet<String> = std::iter::once("go".to_string()).collect();
        let walk = |stop_after| {
            find_callers_batch(&targets, dir.path(), &bloom, None, stop_after, |s| s.len()).unwrap()
        };

        let unbounded = walk(None);
        assert_eq!(unbounded.per_file, [2]);
        assert!(!unbounded.reached_bound);
        assert!(!walk(Some(3)).reached_bound, "2 sites under a bound of 3");
        assert!(walk(Some(2)).reached_bound, "2 sites reach a bound of 2");
    }
}
