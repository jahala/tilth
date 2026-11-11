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

    /// Returns `true` if `"""` and `'''` delimit a string that may hold bare
    /// quotes. A lexer that reads such a string quote by quote loses its place
    /// at the first quote inside it.
    pub(crate) fn has_triple_quoted_strings(self) -> bool {
        crate::lang::spec::spec(self).triple_quoted_strings
    }

    /// Returns `true` if a `#` that starts a word opens a line comment.
    pub(crate) fn has_hash_comments(self) -> bool {
        crate::lang::spec::spec(self).hash_comments
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
    /// For an entry exported under another name (`export { readPlan as
    /// parsePlan }`, whose `name` is `parsePlan`), the name it refers to:
    /// `readPlan`. `None` where the two are the same, and for every other
    /// kind of entry.
    pub local_name: Option<String>,
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
    s.contains(".test.")
        || s.contains(".spec.")
        || s.contains("__tests__/")
        || path.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name.ends_with("_test.go") || (name.starts_with("test_") && name.ends_with(".py"))
        })
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
