//! C language spec. Shares its callee query with C++.

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS};

/// Callee query shared by C and C++.
pub(crate) const CALLEE_QUERY: &str = concat!(
    "(call_expression function: (identifier) @callee)\n",
    "(call_expression function: (field_expression field: (field_identifier) @callee))\n",
);

/// C definition node kinds: functions with a body, and the types a
/// `struct`/`union`/`enum` body or a `typedef` defines. A prototype is a
/// `declaration`, so it is not a definition; neither is a `struct point` with
/// no body, which `extract_definition_name` gives no name.
const C_DEFINITION_KINDS: &[&str] = &[
    "function_definition",
    "struct_specifier",
    "union_specifier",
    "enum_specifier",
    "type_definition",
];

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "C",
    extensions: &["c", "h"],
    filenames: &[],
    grammar: Some(tree_sitter_c::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: None,
    stdlib: StdlibRule::None,
    manifests: &[],
    definition_kinds: C_DEFINITION_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::CppC),
    extract_receiver: None,
    callee_filter: None,
    test_site: None,
    definitions: DEFAULT_DEFS,
};

/// C definitions of every shape the outline, symbol search and grok must see,
/// next to the declarations that must not count as definitions: a forward
/// `struct point;` (line 3), a prototype (line 9) and a bodiless
/// `struct point` used as a return type (line 17). The include guard wraps the
/// type definitions, as it does in a header. Shared by the C tests in
/// `lang::outline`, `search::symbol` and `search::scope`.
#[cfg(test)]
pub(crate) const C_DEFINITIONS_FIXTURE: &str = "#ifndef SHAPES_H
#define SHAPES_H
struct point;
struct point { int x; int y; };
union number { int i; float f; };
enum color { RED, GREEN };
typedef struct point point_t;
typedef int (*compare_fn)(const void *, const void *);
int add(int a, int b);
#endif
int add(int a, int b) {
  return a + b;
}
int *make(void) { return 0; }
static inline const char *label(int x) { return 0; }
int (*pick(void))(int) { return 0; }
struct point *origin(void) { return 0; }
int main(void) { return add(1, 2); }
";
