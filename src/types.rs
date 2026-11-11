use std::path::PathBuf;
use std::time::SystemTime;

/// What kind of query the user issued.
#[derive(Debug)]
pub enum QueryType {
    FilePath(PathBuf),
    Glob(String),
    Symbol(String),
    /// Broad concept query — single lowercase word or multi-word phrase
    /// that likely refers to a feature/module/flow rather than an exact symbol.
    Concept(String),
    Content(String),
    /// Slash-wrapped regex: `/pattern/` → regex content search.
    Regex(String),
    /// Path-like query that didn't resolve — try symbol, then content.
    Fallthrough(String),
}

/// What the output contains — shown in the header bracket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Full,
    Outline,
    Signature,
    Keys,
    // Reserved/roadmap: planned head+tail view mode, not yet wired.
    #[allow(dead_code)]
    HeadTail,
    Empty,
    Generated,
    Minified,
    // Reserved/roadmap: binary file view variant, not yet wired.
    #[allow(dead_code)]
    Binary,
    // Reserved/roadmap: error view variant, not yet wired.
    #[allow(dead_code)]
    Error,
    Section,
    Stripped,
}

impl std::fmt::Display for ViewMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "full"),
            Self::Outline => write!(f, "outline"),
            Self::Signature => write!(f, "signature"),
            Self::Keys => write!(f, "keys"),
            Self::HeadTail => write!(f, "head+tail"),
            Self::Empty => write!(f, "empty"),
            Self::Generated => write!(f, "generated — skipped"),
            Self::Minified => write!(f, "minified — skipped"),
            Self::Binary => write!(f, "skipped"),
            Self::Error => write!(f, "error"),
            Self::Section => write!(f, "section"),
            Self::Stripped => write!(f, "stripped"),
        }
    }
}

/// A single search match, carrying enough context for ranking and display.
#[derive(Debug, Clone)]
pub struct Match {
    pub path: PathBuf,
    pub line: u32,
    pub text: String,
    pub is_definition: bool,
    pub exact: bool,
    pub file_lines: u32,
    pub mtime: SystemTime,
    /// Line range of the enclosing definition node (for expand).
    /// Populated by tree-sitter for definitions; None for usages.
    pub def_range: Option<(u32, u32)>,
    /// The defined symbol name (populated from AST during definition detection).
    pub def_name: Option<String>,
    /// Semantic weight for definition kinds. 0 for usages.
    pub def_weight: u16,
    /// For impl/implements matches: the trait or interface being implemented.
    /// None for primary definitions and plain usages.
    pub impl_target: Option<String>,
}

/// Assembled search results before formatting.
#[derive(Debug)]
pub struct SearchResult {
    pub query: String,
    pub scope: PathBuf,
    pub matches: Vec<Match>,
    pub total_found: usize,
    pub definitions: usize,
    pub usages: usize,
    /// Pre-cap subfacet counts. Computed in `symbol::search` by faceting the
    /// merged set before truncation; used by the renderer to print
    /// `displayed/total` headings and the per-facet hidden-count tail line.
    pub facet_totals: FacetTotals,
    /// Files over `tilth_core::source::MAX_FILE_SIZE` that the walk skipped
    /// even though they contain the query, sorted. The renderer names them,
    /// so a match that lives in a big file is never silently missing.
    pub skipped_large: Vec<PathBuf>,
    /// For a qualified query (`Owner::name`, `Owner.name`) that matched no
    /// definition: the definitions of the bare name that other owners hold,
    /// sorted by path and line. Empty otherwise.
    pub other_owners: Vec<OtherOwner>,
}

/// A definition of a qualified query's bare name that another owner holds,
/// or no owner (`owner: None`, a top-level definition).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OtherOwner {
    pub path: PathBuf,
    pub line: u32,
    pub owner: Option<String>,
}

/// Pre-cap counts per subfacet. Defaults to all-zero for callers that don't
/// facet (`content::search`, `regex` paths) — the renderer renders a bare
/// count when displayed == total, so zero totals never surface a header.
#[derive(Debug, Default, Clone, Copy)]
pub struct FacetTotals {
    pub definitions: usize,
    pub implementations: usize,
    pub tests: usize,
    pub usages_local: usize,
    pub usages_cross: usize,
}

/// Tokens ≈ bytes / 4. Ceiling division, no float.
#[must_use]
pub fn estimate_tokens(byte_len: u64) -> u64 {
    byte_len.div_ceil(4)
}
