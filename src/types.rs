use std::path::PathBuf;
use std::time::SystemTime;

/// What kind of query the user issued.
#[derive(Debug)]
pub enum QueryType {
    FilePath(PathBuf),
    Glob(String),
    Symbol(String),
    Content(String),
    /// Path-like query that didn't resolve — try symbol, then content.
    Fallthrough(String),
}

/// Programming language, carried through the type system so downstream
/// code never re-detects. Adding a language means adding an arm here
/// and the compiler tells you everywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    Java,
    Scala,
    C,
    Cpp,
    Ruby,
    Swift,
    Kotlin,
    CSharp,
    Dockerfile,
    Make,
}

/// File type as detected by extension. Determines outline strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Code(Lang),
    Markdown,
    StructuredData,
    Tabular,
    Log,
    Other,
}

/// What the output contains — shown in the header bracket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Full,
    Outline,
    Keys,
    #[allow(dead_code)]
    HeadTail,
    Empty,
    Generated,
    #[allow(dead_code)]
    Binary,
    #[allow(dead_code)]
    Error,
    Section,
}

impl std::fmt::Display for ViewMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "full"),
            Self::Outline => write!(f, "outline"),
            Self::Keys => write!(f, "keys"),
            Self::HeadTail => write!(f, "head+tail"),
            Self::Empty => write!(f, "empty"),
            Self::Generated => write!(f, "generated — skipped"),
            Self::Binary => write!(f, "skipped"),
            Self::Error => write!(f, "error"),
            Self::Section => write!(f, "section"),
        }
    }
}

/// A single search match, carrying enough context for ranking and display.
#[derive(Debug, Clone)]
pub struct Match {
    pub path: PathBuf,
    pub line: u32,
    #[allow(dead_code)]
    pub column: u32,
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
}

/// A single entry in a code outline.
#[derive(Debug)]
pub struct OutlineEntry {
    pub kind: OutlineKind,
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub children: Vec<OutlineEntry>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineKind {
    Import,
    Function,
    #[allow(dead_code)]
    Method,
    Class,
    Struct,
    Interface,
    TypeAlias,
    Enum,
    Constant,
    Variable,
    ImmutableVariable,
    Export,
    #[allow(dead_code)]
    Property,
    Module,
    #[allow(dead_code)]
    TestSuite,
    #[allow(dead_code)]
    TestCase,
}

/// Tokens ≈ bytes / 4. Ceiling division, no float.
#[must_use]
pub fn estimate_tokens(byte_len: u64) -> u64 {
    byte_len.div_ceil(4)
}

/// UTF-8 safe string truncation. Never panics on multi-byte characters.
#[must_use]
pub fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}
