//! C++ language spec. Shares its callee query with C.

use crate::lang::c::CALLEE_QUERY;
use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS};

/// C++ definition node kinds: the C kinds (macros included) plus classes, namespaces and
/// `using X = ...` aliases. In-class method declarations and prototypes are
/// declarations, not definitions; a method defined out of class
/// (`int Shape::sides() { ... }`) is a `function_definition`.
const CPP_DEFINITION_KINDS: &[&str] = &[
    "function_definition",
    "struct_specifier",
    "union_specifier",
    "enum_specifier",
    "type_definition",
    "class_specifier",
    "namespace_definition",
    "alias_declaration",
    "preproc_def",
    "preproc_function_def",
];

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "C++",
    extensions: &["cpp", "hpp", "cc", "cxx"],
    filenames: &[],
    grammar: Some(tree_sitter_cpp::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: None,
    stdlib: StdlibRule::None,
    manifests: &[],
    definition_kinds: CPP_DEFINITION_KINDS,
    has_lifetimes: false,
    triple_quoted_strings: false,
    hash_comments: false,
    strip_family: Some(StripFamily::CppC),
    extract_receiver: None,
    callee_filter: None,
    test_site: None,
    definitions: DEFAULT_DEFS,
};

/// A token of C or C++ source, as far as [`has_cpp_syntax`] needs one.
#[derive(Clone, Copy, PartialEq)]
enum Token<'a> {
    Ident(&'a str),
    Str(&'a str),
    Punct(u8),
}

/// Split C/C++ source into tokens, skipping comments and the insides of
/// character literals. Each token carries whether it starts its line.
fn tokens(src: &str) -> Vec<(Token<'_>, bool)> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            line_start = true;
            i += 1;
        } else if b.is_ascii_whitespace() {
            i += 1;
        } else if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            while i < bytes.len() && !bytes[i..].starts_with(b"*/") {
                i += 1;
            }
            i += 2;
        } else if b == b'"' || b == b'\'' {
            let start = i + 1;
            i += 1;
            while i < bytes.len() && bytes[i] != b && bytes[i] != b'\n' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            let end = i.min(bytes.len());
            if b == b'"' {
                out.push((Token::Str(&src[start..end]), line_start));
            }
            line_start = false;
            i += 1;
        } else if b.is_ascii_alphanumeric() || b == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push((Token::Ident(&src[start..i]), line_start));
            line_start = false;
        } else {
            if b.is_ascii() {
                out.push((Token::Punct(b), line_start));
            }
            line_start = false;
            i += 1;
        }
    }
    out
}

/// Whether C/C++ source holds syntax only C++ has: a class with a body or a
/// base list, a namespace, a template, an access specifier, a `::` scope, or
/// an `extern "C"` block (C++ syntax that C headers guard for C++
/// includers). C++ keywords used as C identifiers (`struct list *new`) and
/// words in comments or strings do not count.
pub(crate) fn has_cpp_syntax(src: &str) -> bool {
    use Token::{Ident, Punct, Str};
    let toks = tokens(src);
    let at = |i: usize| toks.get(i).map(|t| t.0);
    for (i, &(tok, line_start)) in toks.iter().enumerate() {
        let Ident(word) = tok else { continue };
        let cpp = match word {
            "class" => {
                matches!(at(i + 1), Some(Ident(_)))
                    && toks[i + 2..]
                        .iter()
                        .map(|t| t.0)
                        .find(|t| {
                            matches!(t, Punct(b'{' | b':' | b';' | b'(' | b')' | b'=' | b'}'))
                        })
                        .is_some_and(|t| matches!(t, Punct(b'{' | b':')))
            }
            "namespace" => {
                at(i + 1) == Some(Punct(b'{'))
                    || (matches!(at(i + 1), Some(Ident(_))) && at(i + 2) == Some(Punct(b'{')))
            }
            "template" => at(i + 1) == Some(Punct(b'<')),
            "public" | "private" | "protected" => {
                line_start && at(i + 1) == Some(Punct(b':')) && at(i + 2) != Some(Punct(b':'))
            }
            "extern" => {
                matches!(at(i + 1), Some(Str("C" | "C++"))) && at(i + 2) == Some(Punct(b'{'))
            }
            // `a::b`, but not a C23 attribute `[[gnu::unused]]`.
            _ => {
                at(i + 1) == Some(Punct(b':'))
                    && at(i + 2) == Some(Punct(b':'))
                    && matches!(at(i + 3), Some(Ident(_) | Punct(b'~')))
                    && !(i >= 2 && at(i - 1) == Some(Punct(b'[')) && at(i - 2) == Some(Punct(b'[')))
            }
        };
        if cpp {
            return true;
        }
    }
    false
}

/// C++ definitions of every shape the outline and symbol search must see:
/// a reference return type, in-class methods (destructor, operator), out-of-class
/// `Type::name` definitions, nested namespaces, templates, an `extern "C"`
/// block, a type alias and a scoped enum. The in-class declarations (lines 4
/// and 8), the namespace-level prototype (line 14) and the forward
/// `class Forward;` (line 12) must not count as definitions. Shared by the
/// C++ tests in `lang::outline`, `search::symbol` and `search::scope`.
#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub const CPP_DEFINITIONS_FIXTURE: &str = "int &ref(int &x) { return x; }
class Shape {
 public:
  Shape();
  ~Shape() {}
  int area() const { return 0; }
  bool operator==(const Shape &) const { return true; }
  int sides();
};
Shape::Shape() {}
int Shape::sides() { return 4; }
class Forward;
namespace geo {
namespace detail { int helper(); }
int detail::helper() { return 1; }
template <typename T> T biggest(T a, T b) { return a; }
struct Box {
  int volume() { return 1; }
};
}
template <> int geo::biggest<int>(int a, int b) { return a; }
extern \"C\" { int c_entry(void) { return 0; } }
using Size = unsigned long;
enum class Mode { On, Off };
";
