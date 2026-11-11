//! Every lint exception says how long it stays.
//!
//! An exception is an `#[expect(lint, reason = "...")]`. A temporary one names
//! the issue that removes it, as in `reason = "split by responsibility in
//! #315"`. A permanent one starts its reason with `by design: `. This test
//! parses every Rust file of both crates and fails on any other reason. It
//! reads `#[expect]` and `#[cfg_attr(..., expect(...))]` alike. `syn` sees a
//! macro's body only as tokens, so an attribute written inside a
//! `macro_rules!` body, or in any other macro's input, is not checked.

use std::path::{Path, PathBuf};

use syn::visit::Visit;

/// Both packages, relative to this one's manifest directory.
const PACKAGES: &[&str] = &[".", "crates/tilth-core"];

/// The directories Cargo builds a package's targets from.
const TARGET_DIRS: &[&str] = &["src", "tests", "benches", "examples"];

const PERMANENT: &str = "by design: ";

/// The reason of every `#[expect]` in a file, `None` where it has none or
/// the attribute does not parse.
#[derive(Default)]
struct Expects {
    reasons: Vec<Option<String>>,
}

impl Expects {
    /// Record the reason of `meta` if it is an `expect`, and of every
    /// `expect` that a `cfg_attr` around it would apply.
    fn collect(&mut self, meta: &syn::Meta) {
        if meta.path().is_ident("expect") {
            let reason = meta.require_list().ok().and_then(|list| {
                let mut reason = None;
                let parsed = list.parse_nested_meta(|meta| {
                    if meta.path.is_ident("reason") {
                        reason = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                    }
                    Ok(())
                });
                parsed.ok().and(reason)
            });
            self.reasons.push(reason);
        } else if meta.path().is_ident("cfg_attr") {
            // `cfg_attr(condition, attr, ...)`: everything after the
            // condition is an attribute, and may itself be a `cfg_attr`.
            let parsed = meta.require_list().and_then(|list| {
                list.parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
            });
            match parsed {
                Ok(metas) => metas.iter().skip(1).for_each(|meta| self.collect(meta)),
                // Unreadable, so it may hide an `expect` without a reason.
                Err(_) => self.reasons.push(None),
            }
        }
    }
}

impl Visit<'_> for Expects {
    fn visit_attribute(&mut self, attr: &syn::Attribute) {
        self.collect(&attr.meta);
        syn::visit::visit_attribute(self, attr);
    }
}

/// A reason is valid when it says the exception is permanent or names an
/// issue: a `#` and digits that stand as a word of their own, so `#315` and
/// `(#315)` count and `C#1` does not.
fn is_valid(reason: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    reason.starts_with(PERMANENT)
        || reason.match_indices('#').any(|(at, _)| {
            let before = reason[..at].chars().next_back();
            let after = &reason[at + 1..];
            let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            digits > 0
                && !before.is_some_and(is_word)
                && !after[digits..].chars().next().is_some_and(is_word)
        })
}

/// Every `.rs` file under `dir`, in a stable order.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            rust_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[test]
fn every_expect_reason_names_an_issue_or_is_by_design() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for package in PACKAGES {
        for dir in TARGET_DIRS {
            let dir = root.join(package).join(dir);
            if dir.is_dir() {
                rust_files(&dir, &mut files).expect("list the source files");
            }
        }
    }

    let mut checked = 0;
    let mut wrong = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read a source file");
        let ast =
            syn::parse_file(&text).unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));
        let mut expects = Expects::default();
        expects.visit_file(&ast);
        let shown = file.strip_prefix(root).unwrap_or(file).display();
        for reason in expects.reasons {
            checked += 1;
            match reason {
                Some(reason) if is_valid(&reason) => {}
                Some(reason) => wrong.push(format!("{shown}: {reason:?}")),
                None => wrong.push(format!("{shown}: an #[expect] with no readable reason")),
            }
        }
    }

    assert!(
        checked > 0,
        "no #[expect] found: the scan read the wrong files"
    );
    assert!(
        wrong.is_empty(),
        "a temporary exception's reason names its tracking issue (#123); a permanent \
         one starts with {PERMANENT:?}. These do neither:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn an_issue_reference_is_a_hash_and_digits_standing_alone() {
    for reason in ["split in #315", "(#315)", "see #12.", "#7", "by design: x"] {
        assert!(is_valid(reason), "{reason:?} must count");
    }
    for reason in ["see C#1", "#1a", "item_#2", "a # 3", "#", "no issue"] {
        assert!(!is_valid(reason), "{reason:?} must not count");
    }
}
