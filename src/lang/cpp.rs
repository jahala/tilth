//! C++ language spec. Shares its callee query with C.

use crate::lang::c::CALLEE_QUERY;
use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS};

/// C++ definition node kinds: the C kinds plus classes, namespaces and
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
    strip_family: Some(StripFamily::CppC),
    extract_receiver: None,
    callee_filter: None,
    test_site: None,
    definitions: DEFAULT_DEFS,
};

/// C++ definitions of every shape the outline and symbol search must see:
/// a reference return type, in-class methods (destructor, operator), out-of-class
/// `Type::name` definitions, nested namespaces, templates, an `extern "C"`
/// block, a type alias and a scoped enum. The in-class declarations (lines 4
/// and 8), the namespace-level prototype (line 14) and the forward
/// `class Forward;` (line 12) must not count as definitions. Shared by the
/// C++ tests in `lang::outline`, `search::symbol` and `search::scope`.
#[cfg(test)]
pub(crate) const CPP_DEFINITIONS_FIXTURE: &str = "int &ref(int &x) { return x; }
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
