//! Every lint exception says how long it stays.
//!
//! An exception is an `#[expect(lint, reason = "...")]`. A temporary one names
//! the issue that removes it, as in `reason = "split by responsibility in
//! #315"`. A permanent one starts its reason with `by design: `. This test
//! parses every Rust file of both crates and fails on any other reason.

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

impl Visit<'_> for Expects {
    fn visit_attribute(&mut self, attr: &syn::Attribute) {
        if attr.path().is_ident("expect") {
            let mut reason = None;
            let parsed = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("reason") {
                    reason = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                }
                Ok(())
            });
            self.reasons.push(parsed.ok().and(reason));
        }
        syn::visit::visit_attribute(self, attr);
    }
}

/// A reason is valid when it names an issue (`#` and a digit) or says the
/// exception is permanent.
fn is_valid(reason: &str) -> bool {
    reason.starts_with(PERMANENT)
        || reason
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0] == b'#' && pair[1].is_ascii_digit())
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
