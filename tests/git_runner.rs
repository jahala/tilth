//! Every git command runs through the runner in `src/git.rs`.
//!
//! The runner pins the git settings tilth's parsers depend on, and it passes
//! every revision and path a caller names after `--end-of-options` or `--`,
//! so git never reads one as an option (#277). A `Command::new("git")`
//! anywhere else would skip both. The first test parses every Rust file of
//! both crates and fails on such a call outside the runner. Test code may
//! still spawn git to build its fixtures. Only the literal `"git"` is seen,
//! and `syn` sees a macro's body only as tokens, so a call inside a macro is
//! not checked.

#![expect(
    clippy::disallowed_methods,
    reason = "by design: tests read the files they check directly"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use syn::visit::Visit;

/// Both packages, relative to this one's manifest directory.
const PACKAGES: &[&str] = &[".", "crates/tilth-core"];

/// The one file allowed to start git, relative to the manifest directory.
const RUNNER: &str = "src/git.rs";

/// Counts `Command::new("git")` calls outside test code.
#[derive(Default)]
struct GitSpawns {
    count: usize,
}

/// `#[test]` or `#[cfg(test)]`.
fn is_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || (attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Ident>()
                    .is_ok_and(|ident| ident == "test"))
    })
}

impl Visit<'_> for GitSpawns {
    fn visit_item_mod(&mut self, item: &syn::ItemMod) {
        if !is_test(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &syn::ItemFn) {
        if !is_test(&item.attrs) {
            syn::visit::visit_item_fn(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &syn::ItemImpl) {
        if !is_test(&item.attrs) {
            syn::visit::visit_item_impl(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &syn::ImplItemFn) {
        if !is_test(&item.attrs) {
            syn::visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_expr_call(&mut self, call: &syn::ExprCall) {
        let is_command_new = matches!(&*call.func, syn::Expr::Path(path)
            if path.path.segments.len() >= 2
                && path.path.segments.iter().rev().take(2).map(|s| s.ident.to_string()).eq(["new", "Command"]));
        let is_git = matches!(call.args.first(), Some(syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(name),
            ..
        })) if name.value() == "git");
        if is_command_new && is_git {
            self.count += 1;
        }
        syn::visit::visit_expr_call(self, call);
    }
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
fn only_the_runner_starts_git() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for package in PACKAGES {
        rust_files(&root.join(package).join("src"), &mut files).expect("list the source files");
    }

    let mut in_runner = 0;
    let mut elsewhere = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read a source file");
        let ast =
            syn::parse_file(&text).unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));
        let mut spawns = GitSpawns::default();
        spawns.visit_file(&ast);
        let shown = file.strip_prefix(root).unwrap_or(file);
        if shown == Path::new(RUNNER) {
            in_runner = spawns.count;
        } else if spawns.count > 0 {
            elsewhere.push(format!("{}: {}", shown.display(), spawns.count));
        }
    }

    assert!(
        elsewhere.is_empty(),
        "run git through `crate::git::Git`, which pins its settings and keeps a \
         caller's revision from acting as an option. These files start git \
         themselves:\n{}",
        elsewhere.join("\n")
    );
    assert_eq!(
        in_runner, 1,
        "{RUNNER} starts git in exactly one place; found {in_runner}"
    );
}

/// With no git on `PATH`, `tilth diff` says which git command it could not
/// run, in the words it always has.
#[test]
fn a_missing_git_is_named_in_the_diff_error() {
    let repo = tempfile::tempdir().expect("create tempdir");
    let no_git = tempfile::tempdir().expect("create tempdir");
    let not_found = std::io::Error::from_raw_os_error(2);
    for (args, command) in [
        (&["diff"][..], "rev-parse"),
        (&["diff", "--log", "HEAD~1..HEAD"][..], "rev-parse"),
        (&["diff", "--a", "a.rs", "--b", "b.rs"][..], "diff"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_tilth"))
            .args(args)
            .current_dir(repo.path())
            .env("PATH", no_git.path())
            .output()
            .expect("run tilth");
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("diff error: failed to run git {command}: {not_found}\n"),
            "{args:?}"
        );
    }
}
