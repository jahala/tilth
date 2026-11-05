//! The type alphabet shared by every layer: languages, file types and
//! outline entries.

/// Programming language, carried through the type system so downstream
/// code never re-detects. Adding a language means adding an arm here
/// and the compiler tells you everywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    Php,
    Swift,
    Kotlin,
    CSharp,
    Elixir,
    Bash,
    Dockerfile,
    Make,
}

impl Lang {
    /// Returns `true` if the language uses `'` for lifetime ticks (`'a`,
    /// `'static`) rather than as a string/char delimiter.
    ///
    /// Lexers that scan `'` must disambiguate a lifetime from a single-quoted
    /// literal; only Rust needs the lifetime branch.
    pub(crate) fn has_lifetimes(self) -> bool {
        crate::lang::spec::spec(self).has_lifetimes
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutlineKind {
    Import,
    Function,
    Class,
    Struct,
    Interface,
    TypeAlias,
    Enum,
    Constant,
    Variable,
    ImmutableVariable,
    Export,
    // Property is constructed in lang/outline.rs (property_declaration nodes).
    Property,
    Module,
    // Reserved/roadmap: no tree-sitter grammar currently emits TestSuite nodes.
    #[allow(dead_code)]
    TestSuite,
    // Reserved/roadmap: no tree-sitter grammar currently emits TestCase nodes.
    #[allow(dead_code)]
    TestCase,
}

/// Detect test files by path patterns.
#[must_use]
pub fn is_test_file(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    s.contains(".test.") || s.contains(".spec.") || s.contains("__tests__/")
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
