pub mod format;
pub mod matching;
pub mod overlay;
pub mod parse;

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use rayon::prelude::*;

use tilth_core::types::OutlineKind;

#[derive(Debug)]
pub enum DiffSource {
    GitUncommitted,
    GitStaged,
    GitRef(String),
    Files(PathBuf, PathBuf),
    Patch(PathBuf),
    Log(String),
}

#[derive(Debug)]
pub struct FileDiff {
    pub path: PathBuf,
    pub old_path: Option<PathBuf>,
    pub status: FileStatus,
    pub hunks: Vec<Hunk>,
    pub is_generated: bool,
    pub is_binary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// The line's number in the old file for a removed line, in the new file
    /// otherwise.
    pub line: u32,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug)]
pub struct DiffSymbol {
    pub entry: tilth_core::types::OutlineEntry,
    pub identity: SymbolIdentity,
    /// The enclosing types and modules as shown, such as
    /// `<Foo as fmt::Display>`. Empty at top level.
    pub parent: String,
    pub content_hash: u64,
    pub structural_hash: u64,
    /// Hash of the declaration header's tokens, so a header that is only
    /// reformatted keeps its key.
    pub signature_key: u64,
    pub source_text: String,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct SymbolIdentity {
    pub kind: OutlineKind,
    pub parent_path: String,
    pub name: String,
}

/// First and last line of a symbol, 1-based and inclusive.
pub type Span = (u32, u32);

#[derive(Debug)]
pub struct SymbolChange {
    pub name: String,
    /// The enclosing types and modules, joined by `::`. Empty at top level.
    pub parent: String,
    pub kind: OutlineKind,
    pub change: ChangeType,
    pub match_confidence: MatchConfidence,
    pub old_span: Option<Span>,
    pub new_span: Option<Span>,
    pub old_sig: Option<String>,
    pub new_sig: Option<String>,
    /// The patch lines inside this symbol and inside no smaller changed one.
    pub lines: Vec<DiffLine>,
}

impl SymbolChange {
    /// `Type::method` for a member, the bare name at top level. Two methods
    /// of the same name in different types stay apart.
    #[must_use]
    pub fn qualified_name(&self) -> String {
        if self.parent.is_empty() {
            self.name.clone()
        } else {
            format!("{}::{}", self.parent, self.name)
        }
    }

    /// Whether `name` names this symbol: its qualified name, spelled with
    /// `::` or `.`, or its bare name.
    #[must_use]
    pub fn answers_to(&self, name: &str) -> bool {
        self.name == name || self.qualified_name() == name.replace('.', "::")
    }

    /// The span to show: the new one, or the old one for a symbol that is gone.
    #[must_use]
    pub fn span(&self) -> Span {
        self.new_span.or(self.old_span).unwrap_or((0, 0))
    }
}

#[derive(Debug, Clone)]
pub enum ChangeType {
    Added,
    Deleted,
    BodyChanged,
    SignatureChanged,
    Renamed { old_name: String },
    Moved { old_path: PathBuf },
    RenamedAndMoved { old_name: String, old_path: PathBuf },
    Unchanged,
}

#[derive(Debug, Clone)]
pub enum MatchConfidence {
    Exact,
    Structural,
    Fuzzy(f32),
    Ambiguous(u32),
}

#[derive(Debug)]
pub struct FileOverlay {
    pub path: PathBuf,
    /// The path before a rename.
    pub old_path: Option<PathBuf>,
    pub symbol_changes: Vec<SymbolChange>,
    /// Patch lines outside every changed symbol: all of them for a file
    /// with no outline.
    pub other_lines: Vec<DiffLine>,
    /// Added and removed lines in the file's patch.
    pub added: usize,
    pub removed: usize,
    pub conflicts: Vec<Conflict>,
    pub new_content: Option<String>,
    /// A lockfile or other generated file, summarized by its line counts.
    pub is_generated: bool,
    pub is_binary: bool,
}

#[derive(Debug)]
pub struct Conflict {
    pub line: u32,
    pub ours: String,
    pub theirs: String,
    pub enclosing_fn: Option<String>,
}

#[derive(Debug)]
pub struct CommitSummary {
    pub hash: String,
    pub timestamp: i64,
    pub message: String,
    pub author: String,
    pub overlays: Vec<FileOverlay>,
}

/// Resolve the diff source from CLI/MCP parameters.
///
/// Priority: patch > log > a+b > source > default (uncommitted).
/// Returns an error if only one of `a` or `b` is provided.
pub fn resolve_source(
    source: Option<&str>,
    a: Option<&str>,
    b: Option<&str>,
    patch: Option<&str>,
    log: Option<&str>,
) -> Result<DiffSource, String> {
    // `source` and `log` name a revision or range and go to git as
    // arguments. One that starts with `-` would be read as an option, and
    // `--output=<path>` makes git overwrite that file.
    for (name, value) in [("source", source), ("log", log)] {
        if value.is_some_and(|v| v.starts_with('-')) {
            return Err(format!(
                "{name} must not start with '-': name a revision or range such as \
                 HEAD~1 or main..feature"
            ));
        }
    }
    if let Some(p) = patch {
        return Ok(DiffSource::Patch(PathBuf::from(p)));
    }
    if let Some(l) = log {
        return Ok(DiffSource::Log(l.to_string()));
    }
    match (a, b) {
        (Some(fa), Some(fb)) => return Ok(DiffSource::Files(PathBuf::from(fa), PathBuf::from(fb))),
        (Some(_), None) | (None, Some(_)) => {
            return Err("both --a and --b must be provided together".to_string());
        }
        (None, None) => {}
    }
    if let Some(s) = source {
        let ds = match s {
            "staged" => DiffSource::GitStaged,
            "uncommitted" | "working" => DiffSource::GitUncommitted,
            r => DiffSource::GitRef(expand_parent_shorthand(r)?),
        };
        return Ok(ds);
    }
    Ok(DiffSource::GitUncommitted)
}

/// Spell git's parent shorthands as the range they mean, so the patch and
/// the content of both sides come from the same two revisions: `X^!` and
/// `X^-` are `X^..X`, and `X^-N` is `X^N..X`. `X^@` names every parent of
/// `X` and no second side, so it is refused. Any other revision is kept.
fn expand_parent_shorthand(rev: &str) -> Result<String, String> {
    if let Some(commit) = rev.strip_suffix("^@") {
        return Err(format!(
            "'{rev}' names every parent of a commit; use {commit}^! to see what the commit changed"
        ));
    }
    if let Some(commit) = rev.strip_suffix("^!") {
        return Ok(format!("{commit}^..{commit}"));
    }
    if let Some((commit, n)) = rev.rsplit_once("^-") {
        if n.bytes().all(|b| b.is_ascii_digit()) {
            let n = if n.is_empty() { "1" } else { n };
            return Ok(format!("{commit}^{n}..{commit}"));
        }
    }
    Ok(rev.to_string())
}

/// Where one side of a diff reads a file's content from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Side {
    /// A committed blob: `git show <rev>:<path>`.
    Rev(String),
    /// The staged blob: `git show :<path>`.
    Index,
    /// The file in the working tree.
    WorkingTree,
    /// A file named outside git (`a`/`b` file-to-file diffs).
    File(PathBuf),
    /// No content to read (a patch file names no revisions).
    Unavailable,
}

/// The old and new side of a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sides {
    pub old: Side,
    pub new: Side,
}

/// Decide where each side of `source` reads content from. This is the one
/// place that knows what a revision range means:
/// - `a..b` compares `a` with `b`, and `a...b` compares the merge base of
///   `a` and `b` with `b`, as `git diff` does. An empty end means `HEAD`.
/// - A single revision compares it with the working tree.
/// - Before the first commit, `HEAD` is the empty tree.
fn resolve_sides(source: &DiffSource, repo: Option<&Path>) -> Result<Sides, String> {
    let or_head = |rev: &str| {
        if rev.is_empty() {
            "HEAD".to_string()
        } else {
            rev.to_string()
        }
    };
    Ok(match source {
        DiffSource::GitUncommitted => Sides {
            old: Side::Rev(head_or_empty_tree(repo)?),
            new: Side::WorkingTree,
        },
        DiffSource::GitStaged => Sides {
            old: Side::Rev(head_or_empty_tree(repo)?),
            new: Side::Index,
        },
        DiffSource::GitRef(r) => {
            if let Some((a, b)) = r.split_once("...") {
                let (a, b) = (or_head(a), or_head(b));
                Sides {
                    old: Side::Rev(merge_base(&a, &b, repo)?),
                    new: Side::Rev(b),
                }
            } else if let Some((a, b)) = r.split_once("..") {
                Sides {
                    old: Side::Rev(or_head(a)),
                    new: Side::Rev(or_head(b)),
                }
            } else {
                Sides {
                    old: Side::Rev(r.clone()),
                    new: Side::WorkingTree,
                }
            }
        }
        DiffSource::Files(a, b) => Sides {
            old: Side::File(a.clone()),
            new: Side::File(b.clone()),
        },
        DiffSource::Patch(_) | DiffSource::Log(_) => Sides {
            old: Side::Unavailable,
            new: Side::Unavailable,
        },
    })
}

/// A `git` command that runs in `repo`, or in the process cwd without one.
fn git_command(repo: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(dir) = repo {
        cmd.current_dir(dir);
    }
    cmd
}

/// Run a git command that prints one value, and return it trimmed.
fn git_value(mut cmd: Command, what: &str) -> Result<String, String> {
    let output = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run git {what}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {what} failed: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The repository's top-level directory. Git prints every diff path
/// relative to it, wherever git runs.
fn git_toplevel(repo: Option<&Path>) -> Result<PathBuf, String> {
    let mut cmd = git_command(repo);
    cmd.args(["rev-parse", "--show-toplevel"]);
    git_value(cmd, "rev-parse").map(PathBuf::from)
}

/// `HEAD`, or the empty tree in a repository with no commit yet.
fn head_or_empty_tree(repo: Option<&Path>) -> Result<String, String> {
    let mut cmd = git_command(repo);
    cmd.args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);
    if git_value(cmd, "rev-parse").is_ok() {
        return Ok("HEAD".to_string());
    }
    empty_tree(repo)
}

/// The id of the empty tree, in this repository's hash format.
fn empty_tree(repo: Option<&Path>) -> Result<String, String> {
    let mut cmd = git_command(repo);
    cmd.args(["hash-object", "-t", "tree", "--stdin"]);
    git_value(cmd, "hash-object")
}

fn merge_base(a: &str, b: &str, repo: Option<&Path>) -> Result<String, String> {
    let mut cmd = git_command(repo);
    cmd.args(["merge-base", "--end-of-options", a, b]);
    git_value(cmd, "merge-base")
}

/// Execute a git diff command and return raw unified diff output.
///
/// Git runs inside `repo` when one is provided (the caller's checkout);
/// otherwise in the process cwd. `sides` are `source`'s, from
/// `resolve_sides`: an uncommitted diff compares the working tree with the
/// old side they already name.
fn run_git_diff(source: &DiffSource, sides: &Sides, repo: Option<&Path>) -> Result<String, String> {
    match source {
        DiffSource::Log(_) => {
            return Err("log mode should not call run_git_diff directly".to_string());
        }
        DiffSource::Patch(path) => {
            // A patch may come through a pipe, as `--patch <(git diff)`
            // hands it over, so this read waits for the writer on purpose.
            let content = std::fs::read_to_string(path)
                .map_err(|e| format!("failed to read patch file: {e}"))?;
            return Ok(content);
        }
        _ => {}
    }

    let mut cmd = git_command(repo);
    // Pin the output shape. User git config can otherwise swap in an external
    // diff tool, colour the patch, drop or rename the `a/` `b/` prefixes,
    // make paths cwd-relative, or blank out empty context lines — every one
    // of which the parser misreads as "no changes" or a wrong path (#208).
    cmd.args(["-c", "core.quotePath=false"]);
    cmd.args(["-c", "diff.suppressBlankEmpty=false"]);
    cmd.arg("diff");
    cmd.args([
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-relative",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ]);

    match source {
        DiffSource::GitUncommitted => {
            // working tree vs HEAD (unstaged + staged)
            let Side::Rev(head) = &sides.old else {
                return Err("an uncommitted diff compares with a revision".to_string());
            };
            cmd.arg(head);
        }
        DiffSource::GitStaged => {
            cmd.arg("--staged");
        }
        DiffSource::GitRef(r) => {
            // Whatever the caller passed is a revision, never an option.
            cmd.arg("--end-of-options").arg(r);
        }
        DiffSource::Files(fa, fb) => {
            cmd.arg("--no-index").arg("--").arg(fa).arg(fb);
        }
        // Patch and Log are handled above
        DiffSource::Patch(_) | DiffSource::Log(_) => unreachable!(),
    }

    let output = cmd
        .output()
        .map_err(|e| format!("failed to run git diff: {e}"))?;

    // `git diff --no-index` exits 1 when the files differ; every other variant
    // exits 0 on success. Anything else is a git error, and parsing its empty
    // stdout would report "No changes." for a diff that was never produced.
    let ok = match source {
        DiffSource::Files(..) => matches!(output.status.code(), Some(0 | 1)),
        _ => output.status.success(),
    };
    if !ok {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git diff failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse a patch and build one overlay per file, then match moves across
/// files.
fn build_overlays(raw: &str, sides: &Sides, repo: Option<&Path>) -> Vec<FileOverlay> {
    let file_diffs = parse::parse_unified_diff(raw);
    // Each FileDiff is independent and `compute_overlay` builds its own
    // tree-sitter parsers, so no shared mutable state crosses workers.
    let mut overlays: Vec<FileOverlay> = file_diffs
        .par_iter()
        .map(|fd| overlay::compute_overlay(fd, sides, repo))
        .collect();
    overlay::cross_file_matching(&mut overlays);
    overlays
}

/// Full diff orchestrator — parse → overlay → format pipeline.
///
/// `repo` anchors every git command and working-tree read to the caller's
/// checkout, and a relative `scope` resolves against it. `None` uses the
/// process cwd (CLI, or an MCP call with no `root` arg).
pub fn diff(
    source: &DiffSource,
    repo: Option<&Path>,
    scope: Option<&str>,
    search: Option<&str>,
    blast: bool,
    _expand: usize,
    budget: Option<u64>,
) -> Result<String, String> {
    let base = match repo {
        Some(r) => r.to_path_buf(),
        None => std::env::current_dir().map_err(|e| format!("cannot read the cwd: {e}"))?,
    };
    // Git prints paths from the top of the repository, so every git command
    // and working-tree read runs there, even from a subdirectory.
    let toplevel = match source {
        DiffSource::Files(..) | DiffSource::Patch(_) => None,
        _ => Some(git_toplevel(Some(&base))?),
    };
    let repo = toplevel.as_deref().or(repo);
    let scope = scope.map(|s| ScopeSpec::new(s, &base, toplevel.as_deref()));

    // Log mode has its own pipeline.
    if let DiffSource::Log(range) = source {
        return diff_log(range, repo, scope.as_ref(), budget);
    }

    let sides = resolve_sides(source, repo)?;
    let raw = run_git_diff(source, &sides, repo)?;
    let mut files = build_overlays(&raw, &sides, repo);
    if files.is_empty() && scope.is_none() {
        return Ok("No changes.".to_string());
    }

    // Scope selects one file for a detail view, or the files under a
    // directory for an overview. Moves were matched across every file first,
    // so a symbol moved in from outside the scope still reads as moved.
    let mut detail_fn: Option<&str> = None;
    let mut detail = false;
    if let Some(spec) = &scope {
        match spec.select(files.iter())? {
            Selection::File(i) => {
                files = vec![files.swap_remove(i)];
                detail = true;
                detail_fn = spec.function.as_deref();
            }
            Selection::Files(keep) => {
                if let Some(f) = &spec.function {
                    return Err(spec.function_needs_a_file(f));
                }
                files = files
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, f)| keep.contains(&i).then_some(f))
                    .collect();
            }
            Selection::Nothing => return Ok(format!("No changes in {}.", spec.raw)),
        }
    }
    // Signature warnings count every selected file, before the search
    // filter narrows them.
    let mut warnings = overlay::signature_warnings(&files);

    if let Some(term) = search {
        files.retain_mut(|o| keep_search_matches(o, term));
        if files.is_empty() {
            return Ok(format!("No changes matching '{term}'."));
        }
    }
    let overlays = files;

    if blast {
        // Callers are counted under the caller's root, which may sit below
        // the repository top that git commands run in.
        let mut blast_warnings = compute_blast(&overlays, &base);
        warnings.append(&mut blast_warnings);
    }

    let label = source_label(source);
    let mut output = if detail {
        match detail_fn {
            Some(f) => format::format_function_detail(&overlays[0], f),
            None => format::format_file_detail(&overlays[0], &warnings, budget),
        }
    } else {
        format::format_overview(&overlays, &warnings, &label, budget)
    };

    // Conflict detection for uncommitted diffs.
    if matches!(source, DiffSource::GitUncommitted) {
        let mut all_conflicts = Vec::new();
        for overlay in &overlays {
            let conflicts = overlay::detect_conflicts(&overlay.path, repo);
            if !conflicts.is_empty() {
                all_conflicts.push((&overlay.path, conflicts));
            }
        }
        if !all_conflicts.is_empty() {
            for (path, conflicts) in &all_conflicts {
                output.push('\n');
                output.push_str(&format::format_conflicts(conflicts, path));
            }
            if let Some(b) = budget {
                output = crate::budget::apply(&output, b);
            }
        }
    }

    Ok(output)
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// A `--scope` value: a file or directory, optionally `file:function`.
#[derive(Debug)]
struct ScopeSpec {
    /// The path part as the caller wrote it.
    raw: String,
    /// The path from the top of the repository, when it lies inside one.
    from_top: Option<PathBuf>,
    /// The caller's relative path with `.` parts dropped, matched as a
    /// trailing part of a changed path (`main.rs` for `src/main.rs`).
    suffix: Option<PathBuf>,
    /// The path exists on disk, so a scope with no changes is not a typo.
    exists: bool,
    /// The caller named a directory with a trailing `/`.
    is_dir: bool,
    /// The path is a directory on disk.
    is_dir_on_disk: bool,
    function: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Selection {
    File(usize),
    Files(Vec<usize>),
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    /// The scope is this file (or its path before a rename).
    Exact,
    /// The file lies under the scope directory.
    Under,
    /// The scope is a trailing part of the file's path.
    Suffix,
}

impl ScopeSpec {
    /// `base` is where a relative scope starts: the caller's `root`, or the
    /// cwd. `toplevel` is the repository top, absent for patch and
    /// file-to-file diffs.
    fn new(scope: &str, base: &Path, toplevel: Option<&Path>) -> Self {
        let (path, function) = match scope.split_once(':') {
            Some((p, f)) if !f.is_empty() => (p, Some(f.to_string())),
            _ => (scope, None),
        };
        let given = Path::new(path);
        let absolute = lexical_normalize(&base.join(given));
        let from_top = match toplevel {
            Some(top) => relative_to(&absolute, top),
            None if given.is_relative() => Some(lexical_normalize(given)),
            None => None,
        };
        let suffix = given
            .is_relative()
            .then(|| lexical_normalize(given))
            .filter(|p| !p.as_os_str().is_empty());
        Self {
            raw: path.to_string(),
            from_top,
            suffix,
            exists: absolute.exists(),
            is_dir: path.ends_with('/'),
            is_dir_on_disk: absolute.is_dir(),
            function,
        }
    }

    /// The refusal of `dir:function`: a function is looked up in one file.
    fn function_needs_a_file(&self, function: &str) -> String {
        format!(
            "'{}' is a directory; `{function}` needs a file scope such as path/to/file.rs:{function}",
            self.raw
        )
    }

    fn hit(&self, overlay: &FileOverlay) -> Option<Hit> {
        let paths = || std::iter::once(&overlay.path).chain(overlay.old_path.as_ref());
        if let Some(top) = &self.from_top {
            if top.as_os_str().is_empty() {
                return Some(Hit::Under);
            }
            if !self.is_dir && paths().any(|p| p == top) {
                return Some(Hit::Exact);
            }
            if paths().any(|p| p.starts_with(top)) {
                return Some(Hit::Under);
            }
        }
        match &self.suffix {
            Some(s) if !self.is_dir && paths().any(|p| p.ends_with(s)) => Some(Hit::Suffix),
            _ => None,
        }
    }

    /// Pick the files this scope names. An exact path wins over a directory,
    /// and both win over a trailing-name match, which must be unique.
    fn select<'a>(
        &self,
        overlays: impl Iterator<Item = &'a FileOverlay>,
    ) -> Result<Selection, String> {
        let hits: Vec<(usize, Hit, &FileOverlay)> = overlays
            .enumerate()
            .filter_map(|(i, o)| self.hit(o).map(|h| (i, h, o)))
            .collect();
        let of = |kind: Hit| hits.iter().filter(move |(_, h, _)| *h == kind);
        if let Some((i, _, _)) = of(Hit::Exact).next() {
            return Ok(Selection::File(*i));
        }
        let under: Vec<usize> = of(Hit::Under).map(|(i, _, _)| *i).collect();
        if !under.is_empty() {
            return Ok(Selection::Files(under));
        }
        let suffix: Vec<&(usize, Hit, &FileOverlay)> = of(Hit::Suffix).collect();
        match suffix.as_slice() {
            [(i, _, _)] => return Ok(Selection::File(*i)),
            [] => {}
            many => {
                let names: Vec<String> = many
                    .iter()
                    .map(|(_, _, o)| o.path.display().to_string())
                    .collect();
                return Err(format!(
                    "'{}' matches {} changed files: {}. Give the path from the repository root.",
                    self.raw,
                    names.len(),
                    names.join(", ")
                ));
            }
        }
        if self.exists || self.is_dir {
            return Ok(Selection::Nothing);
        }
        Err(format!("'{}' not found in diff", self.raw))
    }
}

/// Drop `.` parts and resolve `..` against the part before it, without
/// touching the filesystem (a removed directory no longer exists).
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path` relative to `top`, comparing through symlinks: on macOS `/tmp` is
/// `/private/tmp`, and git prints the resolved form. The deepest part of
/// `path` that exists is resolved; the rest (a removed directory) is kept.
fn relative_to(path: &Path, top: &Path) -> Option<PathBuf> {
    let top = std::fs::canonicalize(top).unwrap_or_else(|_| top.to_path_buf());
    let mut existing = path;
    let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
    let resolved = loop {
        if let Ok(p) = std::fs::canonicalize(existing) {
            break p;
        }
        missing.push(existing.file_name()?);
        existing = existing.parent()?;
    };
    let full = missing.iter().rev().fold(resolved, |p, m| p.join(m));
    full.strip_prefix(&top).ok().map(Path::to_path_buf)
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Human-readable label for a diff source.
fn source_label(source: &DiffSource) -> String {
    match source {
        DiffSource::GitUncommitted => "uncommitted".to_string(),
        DiffSource::GitStaged => "staged".to_string(),
        DiffSource::GitRef(r) => r.clone(),
        DiffSource::Files(a, b) => format!("{} vs {}", a.display(), b.display()),
        DiffSource::Patch(p) => format!("patch: {}", p.display()),
        DiffSource::Log(r) => format!("log: {r}"),
    }
}

/// Keep the changed symbols whose name or diff lines contain the search term
/// (case-insensitive). Returns whether the file still has one, or has the
/// term in its lines outside symbols.
fn keep_search_matches(overlay: &mut FileOverlay, term: &str) -> bool {
    let term = term.to_lowercase();
    let has_term = |lines: &[DiffLine]| {
        lines
            .iter()
            .any(|l| l.content.to_lowercase().contains(&term))
    };
    overlay.symbol_changes.retain(|c| {
        !matches!(c.change, ChangeType::Unchanged)
            && (c.qualified_name().to_lowercase().contains(&term) || has_term(&c.lines))
    });
    !overlay.symbol_changes.is_empty() || has_term(&overlay.other_lines)
}

/// Find callers of signature-changed symbols under `scope` and return
/// warnings. `scope` is the caller's root, or the cwd without one.
fn compute_blast(overlays: &[FileOverlay], scope: &Path) -> Vec<String> {
    let sig_changed: HashSet<String> = overlays
        .iter()
        .flat_map(|o| o.symbol_changes.iter())
        .filter(|c| matches!(c.change, ChangeType::SignatureChanged))
        .map(|c| c.name.clone())
        .collect();

    if sig_changed.is_empty() {
        return Vec::new();
    }

    let bloom = tilth_core::index::bloom::BloomFilterCache::new();

    // Stops at the same bound as a write's blast radius, so a diff stays
    // fast on a huge tree; the count then says it is a lower bound.
    let per_file = tilth_core::find_callers_batch(
        &sig_changed,
        scope,
        &bloom,
        None,
        Some(crate::search::blast::BLAST_STOP_AFTER),
        |sites| {
            let mut counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for (target, _) in sites {
                *counts.entry(target).or_default() += 1;
            }
            counts
        },
    );
    let Ok(walk) = per_file else {
        return Vec::new();
    };
    // A file skipped for size may hold callers the counts miss.
    let held = crate::search::names_phrase(sig_changed.iter().map(String::as_str));
    let skipped = crate::search::skipped_files_note(&walk.skipped_large, &held, scope);
    // Sorted by name so the lines come out in the same order every run.
    let at_least = if walk.reached_bound { "at least " } else { "" };
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for file in walk.per_file {
        for (target, n) in file {
            *counts.entry(target).or_default() += n;
        }
    }
    // A walk that stopped may never have reached a symbol's callers.
    if walk.reached_bound {
        for name in sig_changed {
            counts.entry(name).or_default();
        }
    }
    counts
        .into_iter()
        .map(|(name, count)| {
            if count == 0 {
                return format!(
                    "blast: `{name}` signature changed — callers not counted (the search \
                     stopped at {} call sites)",
                    crate::search::blast::BLAST_STOP_AFTER
                );
            }
            format!(
                "blast: `{name}` signature changed — {at_least}{count} caller{} may need updating",
                if count == 1 { "" } else { "s" }
            )
        })
        .chain(skipped.map(|sentence| format!("blast: {sentence}")))
        .collect()
}

/// Log mode pipeline: run per-commit diffs and format as commit summaries.
fn diff_log(
    range: &str,
    repo: Option<&Path>,
    scope: Option<&ScopeSpec>,
    budget: Option<u64>,
) -> Result<String, String> {
    // A directory known before any commit is read is refused once, here,
    // whether or not the range touched it.
    if let Some(spec) = scope {
        if let Some(f) = &spec.function {
            if spec.is_dir || spec.is_dir_on_disk {
                return Err(spec.function_needs_a_file(f));
            }
        }
    }

    // Fields end in NUL so a subject may hold any other character; the
    // subject comes last because only it may be empty or hold spaces.
    let mut cmd = git_command(repo);
    let output = cmd
        .args([
            "log",
            "--format=%H%x00%P%x00%at%x00%an%x00%s",
            "--end-of-options",
            range,
        ])
        .output()
        .map_err(|e| format!("failed to run git log: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git log failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut summaries: Vec<CommitSummary> = Vec::new();
    let mut empty = None;

    for line in stdout.lines() {
        let mut fields = line.splitn(5, '\0');
        let (Some(hash), Some(parents), Some(timestamp), Some(author), Some(message)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };

        // A commit is compared with its first parent; a root commit has
        // none, so it is compared with the empty tree.
        let parent = if let Some(p) = parents.split_whitespace().next() {
            p.to_string()
        } else {
            if empty.is_none() {
                empty = Some(empty_tree(repo)?);
            }
            empty.clone().unwrap_or_default()
        };
        let commit_source = DiffSource::GitRef(format!("{parent}..{hash}"));
        let sides = Sides {
            old: Side::Rev(parent),
            new: Side::Rev(hash.to_string()),
        };
        let raw = run_git_diff(&commit_source, &sides, repo)?;
        let mut overlays = build_overlays(&raw, &sides, repo);
        if let Some(spec) = scope {
            overlays.retain(|o| spec.hit(o).is_some());
            // `file:function` keeps the commits that changed the function,
            // and names only it in them. A directory that is gone from disk
            // shows only in the commits that touched it.
            if let Some(f) = &spec.function {
                if overlays.iter().any(|o| spec.hit(o) == Some(Hit::Under)) {
                    return Err(spec.function_needs_a_file(f));
                }
                for overlay in &mut overlays {
                    overlay
                        .symbol_changes
                        .retain(|c| !matches!(c.change, ChangeType::Unchanged) && c.answers_to(f));
                }
                overlays.retain(|o| !o.symbol_changes.is_empty());
            }
            if overlays.is_empty() {
                continue;
            }
        }

        summaries.push(CommitSummary {
            hash: hash.to_string(),
            timestamp: timestamp.parse().unwrap_or(0),
            message: message.to_string(),
            author: author.to_string(),
            overlays,
        });
    }

    if summaries.is_empty() {
        return Ok("No commits found.".to_string());
    }

    Ok(format::format_log(&summaries, range, budget))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Serializes tests that mutate the process-global cwd. Crate-wide on purpose:
/// two module-local locks cannot protect against each other, and the MCP tool
/// tests (`mcp::tools::diff`) pin cwd too.
#[cfg(test)]
pub(crate) static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// Every git call that takes a caller's revision ends option parsing
    /// before it, so a revision that reaches git without passing through
    /// `resolve_source` still cannot act as an option. `--output=<path>` would
    /// otherwise make git overwrite that file.
    #[test]
    fn git_revision_arguments_cannot_act_as_options() {
        let repo = setup_test_repo();
        let outside = tempfile::tempdir().expect("failed to create tempdir");
        let victim = outside.path().join("victim.txt");
        let injected = format!("--output={}", victim.display());

        std::fs::write(&victim, "keep me\n").unwrap();
        let source = DiffSource::GitRef(injected.clone());
        let sides = resolve_sides(&source, Some(repo.path())).unwrap();
        let _ = run_git_diff(&source, &sides, Some(repo.path()));
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "keep me\n",
            "git diff"
        );

        std::fs::write(&victim, "keep me\n").unwrap();
        let _ = diff_log(&injected, Some(repo.path()), None, None);
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "keep me\n",
            "git log"
        );

        std::fs::write(&victim, "keep me\n").unwrap();
        let _ = overlay::git_show(&injected, Some(repo.path()));
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "keep me\n",
            "git show"
        );
    }

    /// Create a test git repo with an initial commit containing a Rust file.
    fn setup_test_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let p = dir.path();

        git(p, &["init"]);
        git(p, &["config", "user.email", "test@test.com"]);
        git(p, &["config", "user.name", "Test"]);

        let src = p.join("src");
        fs::create_dir_all(&src).unwrap();

        let main_rs = src.join("main.rs");
        fs::write(
            &main_rs,
            "fn hello() {\n    println!(\"hello\");\n}\n\nfn goodbye() {\n    println!(\"bye\");\n}\n\nfn main() {\n    hello();\n    goodbye();\n}\n",
        )
        .unwrap();

        git(p, &["add", "-A"]);
        git(p, &["commit", "-m", "initial"]);

        dir
    }

    /// Run a git command in the given directory.
    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("failed to run git");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Run `diff()` from within the test repo directory, serialized via `CWD_LOCK`.
    /// Passes `repo: None` on purpose — these tests pin the default cwd flow.
    fn run_diff_in(
        dir: &Path,
        source: &DiffSource,
        scope: Option<&str>,
        search: Option<&str>,
        blast: bool,
        budget: Option<u64>,
    ) -> Result<String, String> {
        let _lock = CWD_LOCK.lock().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        let result = diff(source, None, scope, search, blast, 0, budget);
        std::env::set_current_dir(&prev).unwrap();
        result
    }

    // 1. test_empty_diff
    #[test]
    fn test_empty_diff() {
        let dir = setup_test_repo();
        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(result, "No changes.");
    }

    // 2. test_overview_modified
    #[test]
    fn test_overview_modified() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("println!(\"hello\")", "println!(\"hi there\")"),
        )
        .unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(result.contains("[~]"), "expected [~] marker in:\n{result}");
    }

    // 3. test_overview_added
    #[test]
    fn test_overview_added() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let mut content = fs::read_to_string(&main_rs).unwrap();
        content.push_str("\nfn new_function() {\n    println!(\"new\");\n}\n");
        fs::write(&main_rs, content).unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(result.contains("[+]"), "expected [+] marker in:\n{result}");
    }

    // 4. test_overview_deleted
    #[test]
    fn test_overview_deleted() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        // Remove the goodbye function entirely.
        fs::write(
            &main_rs,
            "fn hello() {\n    println!(\"hello\");\n}\n\nfn main() {\n    hello();\n}\n",
        )
        .unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(result.contains("[-]"), "expected [-] marker in:\n{result}");
    }

    // 5. test_overview_signature_changed
    #[test]
    fn test_overview_signature_changed() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        // Change hello() to hello(name: &str)
        let new_content = content
            .replace("fn hello() {", "fn hello(name: &str) {")
            .replace("println!(\"hello\")", "println!(\"hello {}\", name)")
            .replace("hello();", "hello(\"world\");");
        fs::write(&main_rs, new_content).unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(
            result.contains("[~:sig]"),
            "expected [~:sig] marker in:\n{result}"
        );
    }

    /// The blast line of a diff that changes `hello`'s signature, in a repo
    /// where `files` more files call `hello` three times each (and `main`
    /// calls it once).
    fn hello_blast_line(files: usize) -> String {
        let dir = setup_test_repo();
        let calls = "fn caller() {\n    hello();\n    hello();\n    hello();\n}\n";
        for i in 0..files {
            fs::write(dir.path().join(format!("src/c{i:02}.rs")), calls).unwrap();
        }
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-m", "callers"]);
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("fn hello() {", "fn hello(n: u8) {"),
        )
        .unwrap();

        let out = diff(
            &DiffSource::GitUncommitted,
            Some(dir.path()),
            None,
            None,
            true,
            0,
            None,
        )
        .unwrap();
        out.lines()
            .find(|l| l.contains("blast: `hello`"))
            .unwrap_or_else(|| panic!("no blast line in:\n{out}"))
            .to_string()
    }

    #[test]
    fn diff_blast_under_the_bound_gives_the_exact_count() {
        let line = hello_blast_line(4);
        assert!(line.ends_with("— 13 callers may need updating"), "{line}");
    }

    #[test]
    fn diff_blast_that_hits_the_bound_says_at_least() {
        // 40 × 3 + 1 = 121 calls, past the bound the walk stops at.
        let line = hello_blast_line(40);
        assert!(line.contains("— at least "), "{line}");
    }

    // 6. test_file_detail_scope
    #[test]
    fn test_file_detail_scope() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("println!(\"hello\")", "println!(\"hi\")"),
        )
        .unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            Some("src/main.rs"),
            None,
            false,
            None,
        )
        .unwrap();
        assert!(
            result.contains("# Diff: src/main.rs"),
            "expected file detail header in:\n{result}"
        );
        assert!(
            result.contains("symbols touched"),
            "expected symbols touched in:\n{result}"
        );
    }

    // 7. test_function_detail_scope
    #[test]
    fn test_function_detail_scope() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("println!(\"hello\")", "println!(\"hi\")"),
        )
        .unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            Some("src/main.rs:hello"),
            None,
            false,
            None,
        )
        .unwrap();
        assert!(
            result.contains("hello"),
            "expected hello function in:\n{result}"
        );
    }

    // 8. test_staged_diff
    #[test]
    fn test_staged_diff() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("println!(\"hello\")", "println!(\"staged\")"),
        )
        .unwrap();
        git(dir.path(), &["add", "src/main.rs"]);

        let result =
            run_diff_in(dir.path(), &DiffSource::GitStaged, None, None, false, None).unwrap();
        assert!(
            result.contains("main.rs") || result.contains("[~]"),
            "expected staged changes in:\n{result}"
        );
    }

    // 9. test_ref_diff
    #[test]
    fn test_ref_diff() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("println!(\"hello\")", "println!(\"ref\")"),
        )
        .unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-m", "change hello"]);

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitRef("HEAD~1..HEAD".to_string()),
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(
            result.contains("main.rs"),
            "expected main.rs in ref diff:\n{result}"
        );
    }

    // 10. test_generated_file
    #[test]
    fn test_generated_file() {
        let dir = setup_test_repo();
        let lock = dir.path().join("package-lock.json");
        fs::write(&lock, "{}").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-m", "add lock"]);

        fs::write(&lock, "{ \"version\": 2 }").unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(
            result.contains("generated"),
            "expected 'generated' in:\n{result}"
        );
    }

    // 11. test_multiple_files
    #[test]
    fn test_multiple_files() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("println!(\"hello\")", "println!(\"hi\")"),
        )
        .unwrap();

        let lib_rs = dir.path().join("src/lib.rs");
        fs::write(&lib_rs, "pub fn lib_fn() {\n    42\n}\n").unwrap();
        git(dir.path(), &["add", "src/lib.rs"]);
        git(dir.path(), &["commit", "-m", "add lib"]);
        fs::write(&lib_rs, "pub fn lib_fn() {\n    99\n}\n").unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(result.contains("main.rs"), "expected main.rs in:\n{result}");
        assert!(result.contains("lib.rs"), "expected lib.rs in:\n{result}");
        assert!(
            result.contains("2 files"),
            "expected '2 files' in:\n{result}"
        );
    }

    // 12. test_search_filter
    #[test]
    fn test_search_filter() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        // Modify both functions.
        let new_content = content
            .replace("println!(\"hello\")", "println!(\"UNIQUE_MARKER\")")
            .replace("println!(\"bye\")", "println!(\"other change\")");
        fs::write(&main_rs, new_content).unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            None,
            Some("UNIQUE_MARKER"),
            false,
            None,
        )
        .unwrap();
        assert!(
            result.contains("hello"),
            "expected hello (matching) in:\n{result}"
        );
    }

    // 13. test_search_no_matches
    #[test]
    fn test_search_no_matches() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("println!(\"hello\")", "println!(\"hi\")"),
        )
        .unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            None,
            Some("NONEXISTENT_TERM_XYZ"),
            false,
            None,
        )
        .unwrap();
        assert!(
            result.contains("No changes matching"),
            "expected no-match message in:\n{result}"
        );
    }

    // 14. test_file_scope_not_found
    #[test]
    fn test_file_scope_not_found() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("println!(\"hello\")", "println!(\"hi\")"),
        )
        .unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::GitUncommitted,
            Some("nonexistent.rs"),
            None,
            false,
            None,
        );
        assert!(result.is_err(), "expected error for missing file scope");
        assert!(
            result.unwrap_err().contains("not found"),
            "expected 'not found' in error"
        );
    }

    // 15. test_patch_file
    #[test]
    fn test_patch_file() {
        let dir = setup_test_repo();
        let patch = dir.path().join("test.patch");
        let patch_content = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 fn hello() {
-    println!(\"hello\");
+    println!(\"patched\");
 }
";
        fs::write(&patch, patch_content).unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::Patch(patch.clone()),
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(
            result.contains("main.rs"),
            "expected main.rs in patch result:\n{result}"
        );
    }

    // 16. test_file_to_file
    #[test]
    fn test_file_to_file() {
        let dir = setup_test_repo();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        fs::write(&file_a, "line one\nline two\n").unwrap();
        fs::write(&file_b, "line one\nline three\n").unwrap();

        let result = run_diff_in(
            dir.path(),
            &DiffSource::Files(file_a, file_b),
            None,
            None,
            false,
            None,
        )
        .unwrap();
        // The diff should contain something — the files differ.
        assert!(
            !result.contains("No changes"),
            "expected changes between files:\n{result}"
        );
    }

    // 17. test_log_mode
    #[test]
    fn test_log_mode() {
        let dir = setup_test_repo();
        let main_rs = dir.path().join("src/main.rs");

        // Make a second commit.
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("println!(\"hello\")", "println!(\"log test\")"),
        )
        .unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-m", "second commit"]);

        let result = run_diff_in(
            dir.path(),
            &DiffSource::Log("HEAD~1..HEAD".to_string()),
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(
            result.contains("# Log:"),
            "expected log header in:\n{result}"
        );
        assert!(
            result.contains("second commit"),
            "expected commit message in:\n{result}"
        );
    }

    // 18. test_resolve_source_variants
    #[test]
    fn test_resolve_source_variants() {
        // Default → uncommitted.
        assert!(matches!(
            resolve_source(None, None, None, None, None).unwrap(),
            DiffSource::GitUncommitted
        ));

        // Staged.
        assert!(matches!(
            resolve_source(Some("staged"), None, None, None, None).unwrap(),
            DiffSource::GitStaged
        ));

        // Working.
        assert!(matches!(
            resolve_source(Some("working"), None, None, None, None).unwrap(),
            DiffSource::GitUncommitted
        ));

        // Ref.
        match resolve_source(Some("HEAD~3..HEAD"), None, None, None, None).unwrap() {
            DiffSource::GitRef(r) => assert_eq!(r, "HEAD~3..HEAD"),
            other => panic!("expected GitRef, got {other:?}"),
        }

        // Files.
        match resolve_source(None, Some("a.rs"), Some("b.rs"), None, None).unwrap() {
            DiffSource::Files(a, b) => {
                assert_eq!(a, PathBuf::from("a.rs"));
                assert_eq!(b, PathBuf::from("b.rs"));
            }
            other => panic!("expected Files, got {other:?}"),
        }

        // Error: only one of a/b.
        assert!(resolve_source(None, Some("a.rs"), None, None, None).is_err());

        // Patch.
        match resolve_source(None, None, None, Some("test.patch"), None).unwrap() {
            DiffSource::Patch(p) => assert_eq!(p, PathBuf::from("test.patch")),
            other => panic!("expected Patch, got {other:?}"),
        }

        // Log.
        match resolve_source(None, None, None, None, Some("HEAD~5..HEAD")).unwrap() {
            DiffSource::Log(r) => assert_eq!(r, "HEAD~5..HEAD"),
            other => panic!("expected Log, got {other:?}"),
        }

        // Patch takes priority over source.
        assert!(matches!(
            resolve_source(Some("staged"), None, None, Some("x.patch"), None).unwrap(),
            DiffSource::Patch(_)
        ));
    }

    // ── git config must not change the shape of what we parse (#208) ─────────

    /// Stage one change inside `goodbye`, so the patch has a symbol to
    /// attribute the hunk to and a blank context line (the gap between
    /// `hello` and `goodbye`) three lines above it.
    fn stage_goodbye_change(dir: &Path) {
        let main_rs = dir.join("src/main.rs");
        let content = fs::read_to_string(&main_rs).unwrap();
        fs::write(
            &main_rs,
            content.replace("println!(\"bye\")", "println!(\"farewell\")"),
        )
        .unwrap();
        git(dir, &["add", "-A"]);
    }

    fn staged_diff(dir: &Path) -> String {
        run_diff_in(dir, &DiffSource::GitStaged, None, None, false, None).unwrap()
    }

    /// `diff.external` replaces the unified patch with whatever the tool
    /// prints. With `true` that is nothing, which used to read as "No changes.".
    #[test]
    fn staged_diff_bypasses_diff_external() {
        let dir = setup_test_repo();
        git(dir.path(), &["config", "diff.external", "true"]);
        stage_goodbye_change(dir.path());
        let result = staged_diff(dir.path());
        assert!(
            result.contains("goodbye"),
            "external diff tool must be bypassed:\n{result}"
        );
    }

    /// `color.ui=always` wraps the patch in escape codes the parser cannot read.
    #[test]
    fn staged_diff_ignores_color_ui_always() {
        let dir = setup_test_repo();
        git(dir.path(), &["config", "color.ui", "always"]);
        stage_goodbye_change(dir.path());
        let result = staged_diff(dir.path());
        assert!(
            result.contains("goodbye"),
            "colour codes must not reach the parser:\n{result}"
        );
    }

    /// `diff.noprefix` and `diff.mnemonicPrefix` change the `a/` `b/` prefixes
    /// the parser keys on; the file came out as `src/main.rs src/main.rs` or
    /// `c/src/main.rs i/src/main.rs`.
    #[test]
    fn staged_diff_pins_path_prefixes() {
        for (key, value) in [("diff.noprefix", "true"), ("diff.mnemonicPrefix", "true")] {
            let dir = setup_test_repo();
            git(dir.path(), &["config", key, value]);
            stage_goodbye_change(dir.path());
            let result = staged_diff(dir.path());
            assert!(
                result.contains("## src/main.rs")
                    && !result.contains("src/main.rs src/main.rs")
                    && !result.contains("i/src/main.rs"),
                "{key}={value}: path must be unprefixed and repo-relative:\n{result}"
            );
        }
    }

    /// `diff.suppressBlankEmpty` emits a blank context line as "" instead of
    /// " ". The parser drops such lines, shifting every line after them.
    #[test]
    fn run_git_diff_keeps_blank_context_lines() {
        let dir = setup_test_repo();
        git(dir.path(), &["config", "diff.suppressBlankEmpty", "true"]);
        stage_goodbye_change(dir.path());
        let sides = resolve_sides(&DiffSource::GitStaged, Some(dir.path())).unwrap();
        let raw = run_git_diff(&DiffSource::GitStaged, &sides, Some(dir.path())).unwrap();
        assert!(
            raw.lines().any(|l| l == " "),
            "blank context line must survive as a single space:\n{raw}"
        );
    }

    /// `diff.relative` rewrites paths relative to the cwd; the overlay reads
    /// files by repo-root path.
    #[test]
    fn run_git_diff_keeps_repo_relative_paths() {
        let dir = setup_test_repo();
        git(dir.path(), &["config", "diff.relative", "true"]);
        stage_goodbye_change(dir.path());
        let src = dir.path().join("src");
        let sides = resolve_sides(&DiffSource::GitStaged, Some(&src)).unwrap();
        let raw = run_git_diff(&DiffSource::GitStaged, &sides, Some(&src)).unwrap();
        assert!(
            raw.contains("diff --git a/src/main.rs b/src/main.rs"),
            "paths must stay repo-relative when git runs in a subdirectory:\n{raw}"
        );
    }

    /// A git failure used to parse as an empty patch and report "No changes.".
    #[test]
    fn bad_ref_is_an_error_not_no_changes() {
        let dir = setup_test_repo();
        let err = run_diff_in(
            dir.path(),
            &DiffSource::GitRef("no-such-ref".to_string()),
            None,
            None,
            false,
            None,
        )
        .unwrap_err();
        assert!(
            err.contains("no-such-ref"),
            "error must carry git's own message: {err}"
        );
    }

    // ── scopes, attribution, signatures and ranges ───────────────────────────

    /// A repo whose first commit holds `files`. The repo has no other files.
    fn repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let p = dir.path();
        git(p, &["init", "-q"]);
        write_files(p, files);
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "base"]);
        dir
    }

    fn write_files(root: &Path, files: &[(&str, &str)]) {
        for (path, content) in files {
            let full = root.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, content).unwrap();
        }
    }

    fn commit_all(root: &Path, message: &str) {
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", message]);
    }

    fn diff_at(root: &Path, source: &DiffSource, scope: Option<&str>) -> Result<String, String> {
        diff(source, Some(root), scope, None, false, 0, None)
    }

    fn last_commit() -> DiffSource {
        DiffSource::GitRef("HEAD~1..HEAD".to_string())
    }

    /// Three changed files, two of them under `src`, one in a sibling
    /// directory whose name starts with `src`.
    fn repo_with_changes_in_src() -> tempfile::TempDir {
        let dir = repo_with(&[
            ("src/first.rs", "fn first() {\n    1\n}\n"),
            ("src/nested/second.rs", "fn second() {\n    2\n}\n"),
            ("src_other/excluded.rs", "fn excluded() {\n    3\n}\n"),
        ]);
        write_files(
            dir.path(),
            &[
                ("src/first.rs", "fn first() {\n    10\n}\n"),
                ("src/nested/second.rs", "fn second() {\n    20\n}\n"),
                ("src_other/excluded.rs", "fn excluded() {\n    30\n}\n"),
            ],
        );
        dir
    }

    #[test]
    fn directory_scope_lists_the_changed_files_under_it() {
        let dir = repo_with_changes_in_src();
        let absolute = dir.path().join("src");
        for scope in ["src", "src/", "./src", absolute.to_str().unwrap()] {
            let out = diff_at(dir.path(), &DiffSource::GitUncommitted, Some(scope))
                .unwrap_or_else(|e| panic!("scope {scope}: {e}"));
            assert!(
                out.contains("src/first.rs") && out.contains("src/nested/second.rs"),
                "scope {scope} must list both files under src:\n{out}"
            );
            assert!(
                !out.contains("src_other"),
                "scope {scope} must leave out the sibling src_other:\n{out}"
            );
        }
    }

    #[test]
    fn directory_scope_resolves_against_the_working_directory() {
        let dir = repo_with_changes_in_src();
        // The lock is released before any assertion, so a failure here cannot
        // poison it for the other cwd tests.
        let out = {
            let _lock = CWD_LOCK.lock().unwrap();
            let prev = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir.path().join("src")).unwrap();
            let out = diff(
                &DiffSource::GitUncommitted,
                None,
                Some("nested"),
                None,
                false,
                0,
                None,
            );
            std::env::set_current_dir(&prev).unwrap();
            out
        };
        let out = out.unwrap();
        assert!(
            out.contains("src/nested/second.rs") && !out.contains("src/first.rs"),
            "`nested` from inside src means src/nested:\n{out}"
        );
        assert!(
            out.contains("[~]") && out.contains("second"),
            "the file read from a subdirectory must still be outlined:\n{out}"
        );
    }

    #[test]
    fn scope_follows_renamed_and_removed_files() {
        let dir = repo_with(&[
            ("old/a.rs", "fn moved() {\n    4\n}\n"),
            ("gone/a.rs", "fn gone() {\n    5\n}\n"),
        ]);
        git(dir.path(), &["mv", "old", "new"]);
        git(dir.path(), &["rm", "-q", "-r", "gone"]);
        commit_all(dir.path(), "move from old");

        let out = diff_at(dir.path(), &last_commit(), Some("old")).unwrap();
        assert!(out.contains("new/a.rs"), "rename source dir:\n{out}");
        let out = diff_at(dir.path(), &last_commit(), Some("gone/")).unwrap();
        assert!(out.contains("gone/a.rs"), "removed dir:\n{out}");
        assert!(
            out.contains("[-]"),
            "removed dir shows the deletion:\n{out}"
        );

        let log = diff_at(
            dir.path(),
            &DiffSource::Log("HEAD~1..HEAD".into()),
            Some("old"),
        )
        .unwrap();
        assert!(
            log.contains("move from old"),
            "log with a dir scope:\n{log}"
        );
    }

    #[test]
    fn scope_on_an_unchanged_directory_says_no_changes() {
        let dir = repo_with_changes_in_src();
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        let out = diff_at(dir.path(), &DiffSource::GitUncommitted, Some("docs")).unwrap();
        assert!(out.contains("No changes"), "{out}");
        let err = diff_at(dir.path(), &DiffSource::GitUncommitted, Some("nowhere")).unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn scope_that_matches_several_files_by_name_is_ambiguous() {
        let dir = repo_with(&[
            ("a/mod.rs", "fn a() {\n    1\n}\n"),
            ("b/mod.rs", "fn b() {\n    1\n}\n"),
        ]);
        write_files(
            dir.path(),
            &[
                ("a/mod.rs", "fn a() {\n    2\n}\n"),
                ("b/mod.rs", "fn b() {\n    2\n}\n"),
            ],
        );
        let err = diff_at(dir.path(), &DiffSource::GitUncommitted, Some("mod.rs")).unwrap_err();
        assert!(
            err.contains("a/mod.rs") && err.contains("b/mod.rs"),
            "{err}"
        );
        let out = diff_at(dir.path(), &DiffSource::GitUncommitted, Some("a/mod.rs")).unwrap();
        assert!(out.contains("# Diff: a/mod.rs"), "{out}");
    }

    #[test]
    fn scoped_detail_keeps_removed_lines_of_a_text_file() {
        let dir = repo_with(&[("notes.txt", "one\ntwo\n")]);
        write_files(dir.path(), &[("notes.txt", "one\nthree\n")]);
        commit_all(dir.path(), "edit notes");
        let out = diff_at(dir.path(), &last_commit(), Some("notes.txt")).unwrap();
        assert!(out.contains("+1/\u{2212}1 lines"), "{out}");
        assert!(out.contains("-   2| two"), "removed line:\n{out}");
        assert!(out.contains("+   2| three"), "added line:\n{out}");
    }

    #[test]
    fn scoped_detail_keeps_removed_lines_of_a_function() {
        let dir = repo_with(&[(
            "src/lib.rs",
            "fn keep() {\n    1\n}\n\nfn edit() {\n    let a = 1;\n    let b = 2;\n    a + b\n}\n",
        )]);
        write_files(
            dir.path(),
            &[(
                "src/lib.rs",
                "fn keep() {\n    1\n}\n\nfn edit() {\n    let a = 1;\n    let b = 3;\n    a + b\n}\n",
            )],
        );
        let out = diff_at(dir.path(), &DiffSource::GitUncommitted, Some("src/lib.rs")).unwrap();
        assert!(out.contains("+1/\u{2212}1 lines"), "{out}");
        // Real file line numbers, not counted from the symbol's first line.
        assert!(
            out.contains("-   7|     let b = 2;"),
            "removed line:\n{out}"
        );
        assert!(out.contains("+   7|     let b = 3;"), "added line:\n{out}");
    }

    const TWO_RUNS: &str = "class A {\n  run() {\n    const value = 1;\n    return value;\n  }\n}\n\nclass B {\n  run() {\n    const value = 1;\n    return value;\n  }\n}\n";

    fn repo_with_two_runs_changed() -> tempfile::TempDir {
        let dir = repo_with(&[("src/service.ts", TWO_RUNS)]);
        let changed =
            TWO_RUNS
                .replacen("value = 1", "value = 2", 1)
                .replacen("value = 1", "value = 3", 1);
        write_files(dir.path(), &[("src/service.ts", &changed)]);
        commit_all(dir.path(), "change both runs");
        dir
    }

    #[test]
    fn same_named_methods_keep_their_own_changes() {
        let dir = repo_with_two_runs_changed();
        let out = diff_at(dir.path(), &last_commit(), Some("src/service.ts")).unwrap();
        let section = |name: &str| {
            let start = out
                .find(&format!("] {name} —"))
                .unwrap_or_else(|| panic!("no {name} section:\n{out}"));
            let rest = &out[start..];
            let end = rest[1..].find("\n## ").map_or(rest.len(), |i| i + 1);
            rest[..end].to_string()
        };
        let a = section("A::run");
        let b = section("B::run");
        assert!(a.contains("value = 2") && !a.contains("value = 3"), "{a}");
        assert!(b.contains("value = 3") && !b.contains("value = 2"), "{b}");
        // Each changed line shows once, under its innermost symbol.
        assert_eq!(out.matches("+   3|").count(), 1, "{out}");
        assert!(out.contains("+2/\u{2212}2 lines"), "{out}");
    }

    #[test]
    fn diff_lists_symbols_in_file_order() {
        let dir = repo_with_two_runs_changed();
        let out = diff_at(dir.path(), &last_commit(), None).unwrap();
        let names: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with("  ["))
            .filter_map(|l| l.split_whitespace().nth(1))
            .collect();
        assert_eq!(names, ["A", "A::run", "B", "B::run"], "{out}");
        let detail = diff_at(dir.path(), &last_commit(), Some("src/service.ts")).unwrap();
        for _ in 0..5 {
            assert_eq!(
                diff_at(dir.path(), &last_commit(), Some("src/service.ts")).unwrap(),
                detail
            );
        }
    }

    #[test]
    fn formatting_a_signature_is_not_a_signature_change() {
        let dir = repo_with(&[(
            "src/fmt.ts",
            "class Service {\n  run(value: string, count: number): string | number {\n    return value;\n  }\n}\n",
        )]);
        write_files(
            dir.path(),
            &[(
                "src/fmt.ts",
                "class Service {\n  run(\n    value: /* text */ string,\n    count: number,\n  ):\n    | string\n    | number {\n    return value;\n  }\n}\n",
            )],
        );
        let out = diff_at(dir.path(), &DiffSource::GitUncommitted, None).unwrap();
        assert!(!out.contains("[~:sig]"), "{out}");
        assert!(!out.contains("signature changed"), "{out}");
        assert!(out.contains("Service::run"), "{out}");
    }

    #[test]
    fn multiline_signature_change_is_a_signature_change() {
        let old = "class Service {\n  run(\n    value: string\n  ): { result: string } {\n    return { result: value };\n  }\n}\n";
        let dir = repo_with(&[("src/ml.ts", old)]);
        write_files(
            dir.path(),
            &[("src/ml.ts", &old.replace("value: string", "value: number"))],
        );
        let out = diff_at(dir.path(), &DiffSource::GitUncommitted, Some("src/ml.ts")).unwrap();
        assert!(out.contains("[~:sig] Service::run"), "{out}");
        assert!(
            out.contains("BEFORE: run(value: string): { result: string }"),
            "{out}"
        );
        assert!(
            out.contains("AFTER:  run(value: number): { result: string }"),
            "{out}"
        );
    }

    #[test]
    fn signature_change_names_the_symbol_once() {
        let dir = repo_with(&[(
            "src/sub.cpp",
            "int sub(int a, int b) {\n    return a - b;\n}\n",
        )]);
        write_files(
            dir.path(),
            &[(
                "src/sub.cpp",
                "int sub(int a, int b, int c) {\n    return a - b - c;\n}\n",
            )],
        );
        let out = diff_at(dir.path(), &DiffSource::GitUncommitted, None).unwrap();
        let line = out
            .lines()
            .find(|l| l.contains("[~:sig]"))
            .unwrap_or_else(|| panic!("no signature line:\n{out}"));
        assert_eq!(line.matches("sub(").count(), 2, "{line}");
        assert!(
            line.contains("int sub(int a, int b) → int sub(int a, int b, int c)"),
            "{line}"
        );
    }

    #[test]
    fn three_dot_range_compares_from_the_merge_base() {
        let dir = repo_with(&[(
            "src/a.rs",
            "fn alpha() {\n    1\n}\n\nfn beta() {\n    2\n}\n",
        )]);
        git(dir.path(), &["branch", "-q", "base"]);
        git(dir.path(), &["checkout", "-q", "-b", "feat"]);
        write_files(
            dir.path(),
            &[(
                "src/a.rs",
                "fn alpha() {\n    100\n}\n\nfn beta() {\n    2\n}\n\nfn gamma() {\n    3\n}\n",
            )],
        );
        commit_all(dir.path(), "feat");
        git(dir.path(), &["checkout", "-q", "base"]);
        write_files(dir.path(), &[("other.txt", "other\n")]);
        commit_all(dir.path(), "base moves on");

        let out = diff_at(dir.path(), &DiffSource::GitRef("base...feat".into()), None).unwrap();
        assert!(out.contains("[~]") && out.contains("alpha"), "{out}");
        assert!(out.contains("[+]") && out.contains("gamma"), "{out}");
        assert!(
            !out.contains("other.txt"),
            "three dots start at the merge base:\n{out}"
        );
    }

    #[test]
    fn repo_without_commits_diffs_against_nothing() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        write_files(dir.path(), &[("fresh.rs", "fn fresh() {\n    1\n}\n")]);
        git(dir.path(), &["add", "fresh.rs"]);
        for source in [DiffSource::GitUncommitted, DiffSource::GitStaged] {
            let out = diff_at(dir.path(), &source, None).unwrap();
            assert!(out.contains("[+]") && out.contains("fresh"), "{out}");
        }
    }

    #[test]
    fn log_includes_the_root_commit() {
        let dir = repo_with(&[("src/a.rs", "fn alpha() {\n    1\n}\n")]);
        write_files(dir.path(), &[("src/a.rs", "fn alpha() {\n    2\n}\n")]);
        commit_all(dir.path(), "second");
        let out = diff_at(dir.path(), &DiffSource::Log("HEAD".into()), None).unwrap();
        assert!(out.contains("2 commits"), "{out}");
        assert!(
            out.contains("\"base\"") && out.contains("[+] alpha"),
            "{out}"
        );
    }

    /// The search filter drops files; each kept file keeps its own
    /// generated/binary label, not the label of a dropped neighbour.
    #[test]
    fn search_keeps_each_file_with_its_own_label() {
        let dir = repo_with(&[
            ("package-lock.json", "{\n  \"a\": 1\n}\n"),
            ("src/lib.rs", "fn lib_fn() {\n    1\n}\n"),
        ]);
        write_files(
            dir.path(),
            &[
                ("package-lock.json", "{\n  \"a\": 2\n}\n"),
                ("src/lib.rs", "fn lib_fn() {\n    UNIQUE_MARKER\n}\n"),
            ],
        );
        let out = diff(
            &DiffSource::GitUncommitted,
            Some(dir.path()),
            None,
            Some("UNIQUE_MARKER"),
            false,
            0,
            None,
        )
        .unwrap();
        assert!(out.contains("## src/lib.rs (1 symbols)"), "{out}");
        assert!(!out.contains("generated"), "{out}");
    }

    #[test]
    fn generated_file_counts_its_changed_lines() {
        let dir = repo_with(&[("package-lock.json", "{\n  \"a\": 1\n}\n")]);
        write_files(
            dir.path(),
            &[("package-lock.json", "{\n  \"a\": 2,\n  \"b\": 3\n}\n")],
        );
        let out = diff_at(dir.path(), &DiffSource::GitUncommitted, None).unwrap();
        assert!(out.contains("generated, 3 lines changed"), "{out}");
    }

    /// `file:name` picks the one changed symbol of that name. An unchanged
    /// symbol of the same name in another type is no rival.
    #[test]
    fn function_scope_ignores_unchanged_symbols_of_the_same_name() {
        let dir = repo_with(&[("src/service.ts", TWO_RUNS)]);
        let changed = TWO_RUNS.replacen("value = 1", "value = 2", 1);
        write_files(dir.path(), &[("src/service.ts", &changed)]);
        let out = diff_at(
            dir.path(),
            &DiffSource::GitUncommitted,
            Some("src/service.ts:run"),
        )
        .unwrap();
        assert!(!out.contains("ambiguous"), "{out}");
        assert!(out.contains("A::run") && out.contains("value = 2"), "{out}");
    }

    /// `X^!` is the change `X` made, and `X^-1` the same: git reads both as
    /// the range from `X`'s first parent to `X`. Read as one revision
    /// compared with the working tree, no symbol could be matched.
    #[test]
    fn parent_shorthands_name_the_commit_s_own_change() {
        let dir = repo_with(&[("src/a.rs", "fn alpha() {\n    1\n}\n")]);
        write_files(dir.path(), &[("src/a.rs", "fn alpha() {\n    2\n}\n")]);
        commit_all(dir.path(), "second");
        // A later, uncommitted edit must not leak into the commit's diff.
        write_files(dir.path(), &[("src/a.rs", "fn alpha() {\n    3\n}\n")]);
        let range = diff_at(dir.path(), &last_commit(), None).unwrap();
        assert!(range.contains("[~]      alpha"), "{range}");
        for spec in ["HEAD^!", "HEAD^-1", "HEAD^-"] {
            let source = resolve_source(Some(spec), None, None, None, None).unwrap();
            let out = diff_at(dir.path(), &source, None).unwrap();
            assert!(out.contains("[~]      alpha"), "{spec}:\n{out}");
            assert!(out.contains("1 file"), "{spec}:\n{out}");
        }
        // `X^@` names every parent of X, which compares nothing with nothing
        // the way a two-sided diff does.
        let err = resolve_source(Some("HEAD^@"), None, None, None, None).unwrap_err();
        assert!(err.contains("^!"), "{err}");
    }

    /// When the blast search stops at its bound, a changed symbol it found
    /// no call for may have callers it never reached. It says so, where no
    /// line would read as "no callers".
    #[test]
    fn diff_blast_that_stopped_says_which_symbols_it_did_not_count() {
        let calls = "fn caller() {\n    hello();\n    hello();\n    hello();\n}\n";
        let mut files = vec![(
            "src/main.rs".to_string(),
            "fn hello() {\n    1;\n}\n\nfn quiet() {\n    2;\n}\n".to_string(),
        )];
        for i in 0..40 {
            files.push((format!("src/c{i:02}.rs"), calls.to_string()));
        }
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();
        let dir = repo_with(&files);
        write_files(
            dir.path(),
            &[(
                "src/main.rs",
                "fn hello(n: u8) {\n    1;\n}\n\nfn quiet(n: u8) {\n    2;\n}\n",
            )],
        );
        let out = diff(
            &DiffSource::GitUncommitted,
            Some(dir.path()),
            None,
            None,
            true,
            0,
            None,
        )
        .unwrap();
        assert!(
            out.contains("blast: `hello` signature changed — at least "),
            "{out}"
        );
        assert!(
            out.contains("blast: `quiet` signature changed — callers not counted"),
            "{out}"
        );
    }

    /// A move keeps its owner: `A::run` gone from one file and `B::run` new
    /// in another are a deletion and an addition, while a top-level
    /// function that changes file is a move.
    #[test]
    fn a_move_across_files_keeps_the_symbol_s_owner() {
        let dir = repo_with(&[
            ("a.ts", "class A {\n  run() {\n    return 1;\n  }\n}\n\nfunction helper() {\n  return 2;\n}\n"),
            ("b.ts", "class B {\n  stop() {\n    return 3;\n  }\n}\n"),
        ]);
        write_files(
            dir.path(),
            &[
                ("a.ts", "class A {\n}\n"),
                (
                    "b.ts",
                    "class B {\n  stop() {\n    return 3;\n  }\n  run() {\n    return 1;\n  }\n}\n\nfunction helper() {\n  return 2;\n}\n",
                ),
            ],
        );
        let out = diff_at(dir.path(), &DiffSource::GitUncommitted, None).unwrap();
        assert!(out.contains("[-]      A::run"), "{out}");
        assert!(out.contains("[+]      B::run"), "{out}");
        assert!(out.contains("[↦]      helper"), "{out}");
    }

    /// A file over the size cap is not searched for callers; when it names
    /// a changed symbol, the blast says so.
    #[test]
    fn diff_blast_names_a_big_file_it_did_not_search() {
        let limit = usize::try_from(tilth_core::source::MAX_FILE_SIZE).unwrap();
        let pad = "// padding line to push the file over the size limit\n".repeat(limit / 50 + 1);
        let big = format!("{pad}fn b() {{\n    hello();\n}}\n");
        let dir = repo_with(&[
            ("src/main.rs", "fn hello() {\n    1;\n}\n"),
            ("src/big.rs", &big),
        ]);
        write_files(
            dir.path(),
            &[("src/main.rs", "fn hello(n: u8) {\n    1;\n}\n")],
        );
        let out = diff(
            &DiffSource::GitUncommitted,
            Some(dir.path()),
            None,
            None,
            true,
            0,
            None,
        )
        .unwrap();
        assert!(
            out.contains(
                "⚠ blast: 1 file over 500 KB contains `hello` and was not searched: src/big.rs"
            ),
            "{out}"
        );
    }

    /// Blast counts the callers under the caller's root, not under the whole
    /// repository the root sits in.
    #[test]
    fn diff_blast_counts_callers_under_root_only() {
        let calls = "fn caller() {\n    hello();\n}\n";
        let dir = repo_with(&[
            ("app/src/main.rs", "fn hello() {\n    1;\n}\n"),
            ("app/src/c.rs", calls),
            ("other/c.rs", calls),
        ]);
        write_files(
            dir.path(),
            &[("app/src/main.rs", "fn hello(n: u8) {\n    1;\n}\n")],
        );
        let out = diff(
            &DiffSource::GitUncommitted,
            Some(&dir.path().join("app")),
            None,
            None,
            true,
            0,
            None,
        )
        .unwrap();
        assert!(
            out.contains("blast: `hello` signature changed — 1 caller may need updating"),
            "{out}"
        );
    }

    /// In log mode `file:function` keeps the commits that changed that
    /// function, and names only it.
    #[test]
    fn log_function_scope_keeps_commits_that_changed_the_function() {
        let dir = repo_with(&[(
            "src/a.rs",
            "fn alpha() {\n    1\n}\n\nfn beta() {\n    1\n}\n",
        )]);
        write_files(
            dir.path(),
            &[(
                "src/a.rs",
                "fn alpha() {\n    2\n}\n\nfn beta() {\n    1\n}\n",
            )],
        );
        commit_all(dir.path(), "touch alpha");
        write_files(
            dir.path(),
            &[(
                "src/a.rs",
                "fn alpha() {\n    2\n}\n\nfn beta() {\n    2\n}\n",
            )],
        );
        commit_all(dir.path(), "touch beta");
        let out = diff_at(
            dir.path(),
            &DiffSource::Log("HEAD~2..HEAD".into()),
            Some("src/a.rs:beta"),
        )
        .unwrap();
        assert!(out.contains("1 commit,"), "{out}");
        assert!(
            out.contains("touch beta") && !out.contains("touch alpha"),
            "{out}"
        );
        assert!(out.contains("[~] beta") && !out.contains("alpha"), "{out}");
        let err = diff_at(
            dir.path(),
            &DiffSource::Log("HEAD~2..HEAD".into()),
            Some("src/:beta"),
        )
        .unwrap_err();
        assert!(err.contains("needs a file scope"), "{err}");
    }

    /// `dir:function` is refused in log mode whether or not a commit of the
    /// range touched the directory. Checked inside the commit loop, it was
    /// refused only then, and a range that never touched it logged nothing.
    #[test]
    fn log_refuses_a_function_in_a_directory_the_range_never_touched() {
        let dir = repo_with(&[
            ("src/a.rs", "fn alpha() {\n    1\n}\n"),
            ("lib/b.rs", "fn beta() {\n    1\n}\n"),
        ]);
        write_files(dir.path(), &[("src/a.rs", "fn alpha() {\n    2\n}\n")]);
        commit_all(dir.path(), "touch alpha");
        for scope in ["lib:beta", "lib/:beta"] {
            let err = diff_at(
                dir.path(),
                &DiffSource::Log("HEAD~1..HEAD".into()),
                Some(scope),
            )
            .unwrap_err();
            assert!(err.contains("needs a file scope"), "{scope}: {err}");
        }
    }
}
