use crate::lang::treesitter::{
    extract_definition_name, is_or_body_of, is_type_reference, node_text_simple, NodeTextMode,
    NAMESPACE_KINDS, PASS_THROUGH_KINDS,
};
use crate::types::{Lang, OutlineEntry, OutlineKind};

/// Get the tree-sitter Language for a given Lang variant.
pub fn outline_language(lang: Lang) -> Option<tree_sitter::Language> {
    crate::lang::spec::spec(lang).grammar.map(Into::into)
}

/// Parse markdown content into a tree-sitter block tree.
///
/// Returns `None` if the parser fails to set the language (should not happen
/// in practice). The block grammar is what tilth's outline / definition
/// scanners need: it emits `atx_heading`, `setext_heading`, `section`, and
/// `fenced_code_block` nodes. Inline structure (emphasis, links inside the
/// heading text) is parsed by a separate inline grammar tilth doesn't use —
/// heading text is read as the raw inline node's text.
///
/// Centralised so both `read::outline::markdown` and
/// `search::symbol::find_defs_markdown_buf` configure the parser the same
/// way.
#[must_use]
pub fn parse_markdown(content: &str) -> Option<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_md::LANGUAGE.into()).ok()?;
    parser.parse(content, None)
}

/// Map an `atx_heading` or `setext_heading` node to its 1-6 level by
/// inspecting the marker child. Returns `None` for malformed nodes.
#[must_use]
pub fn heading_level(node: tree_sitter::Node) -> Option<u8> {
    let kind = node.kind();
    if kind == "atx_heading" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "atx_h1_marker" => return Some(1),
                "atx_h2_marker" => return Some(2),
                "atx_h3_marker" => return Some(3),
                "atx_h4_marker" => return Some(4),
                "atx_h5_marker" => return Some(5),
                "atx_h6_marker" => return Some(6),
                _ => {}
            }
        }
        None
    } else if kind == "setext_heading" {
        // setext H1: `=====`; H2: `-----`. Marker is a child node.
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "setext_h1_underline" => return Some(1),
                "setext_h2_underline" => return Some(2),
                _ => {}
            }
        }
        None
    } else {
        None
    }
}

/// Read the heading text of an `atx_heading` / `setext_heading` node from
/// pre-split source lines. Returns the inline content with surrounding
/// whitespace + trailing `#`s (for ATX-closed headings like `## Foo ##`)
/// trimmed, matching the previous hand-rolled scanner's output.
#[must_use]
pub fn heading_text(node: tree_sitter::Node, lines: &[&str]) -> String {
    // Both heading kinds expose their inline content as an `inline` child.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "inline" {
            let text = node_text_simple(child, lines, NodeTextMode::Full);
            return text.trim().trim_end_matches('#').trim().to_string();
        }
    }
    String::new()
}

/// Walk top-level children of the root node, extracting outline entries.
#[must_use]
pub fn walk_top_level(root: tree_sitter::Node, lines: &[&str], lang: Lang) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if lang == Lang::Go {
            if let Some(specs) = go_declaration_entries(child, lines) {
                entries.extend(specs);
                continue;
            }
        }
        if matches!(lang, Lang::TypeScript | Lang::Tsx | Lang::JavaScript) {
            if let Some(specifiers) = export_clause_entries(child, lines) {
                entries.extend(specifiers);
                continue;
            }
        }
        push_entries(child, lines, lang, 0, &mut entries);
    }

    entries
}

/// Push the outline entry for `node`. A pass-through wrapper (`#ifdef`,
/// `extern "C" { ... }`, `template <...>`) has no entry of its own: the
/// entries of what it wraps take its place.
fn push_entries(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: Lang,
    depth: usize,
    out: &mut Vec<OutlineEntry>,
) {
    if is_or_body_of(node, node.parent(), PASS_THROUGH_KINDS) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            push_entries(child, lines, lang, depth, out);
        }
    } else if let Some(entry) = node_to_entry(node, lines, lang, depth) {
        out.push(entry);
    }
}

/// Convert a tree-sitter node to an `OutlineEntry` based on its kind.
#[expect(
    clippy::too_many_lines,
    reason = "one name extractor replaces the per-kind branches in #310"
)]
fn node_to_entry(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: Lang,
    depth: usize,
) -> Option<OutlineEntry> {
    // A keyword token is never an entry: Ruby's `class` keyword has the kind
    // of the class it opens.
    if !node.is_named() {
        return None;
    }
    let kind_str = node.kind();
    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    let (kind, name, signature) = match kind_str {
        // Functions
        "function_declaration"
        | "function_definition"
        | "function_item"
        | "generator_function_declaration"
        | "method_definition"
        | "method_declaration"
        | "constructor_declaration"
        | "init_declaration"
        | "deinit_declaration"
        | "protocol_function_declaration"
        | "abstract_method_signature"
        | "method"
        | "singleton_method" => {
            let name = extract_definition_name(node, lines).unwrap_or_else(|| {
                // Swift deinit has no name field — use the node kind as name
                if kind_str == "deinit_declaration" {
                    "deinit".into()
                } else {
                    "<anonymous>".into()
                }
            });
            let sig = extract_signature(node, lines);
            (OutlineKind::Function, name, Some(sig))
        }

        // Classes & structs. `class` is Ruby's; a JS/TS class expression
        // never sits where outline entries are read.
        "class_declaration" | "class_definition" | "abstract_class_declaration" | "class" => {
            let name = find_child_text(node, "name", lines)
                .or_else(|| find_child_text(node, "identifier", lines))
                .unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::Class, name, None)
        }
        "struct_item" | "struct_declaration" => {
            let name = find_child_text(node, "name", lines).unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::Struct, name, None)
        }

        // C/C++ types. Only a specifier with a body defines one; a bare
        // `struct point` or `class Forward;` names a type defined elsewhere.
        "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier" => {
            if is_type_reference(node) {
                return None;
            }
            let name = extract_definition_name(node, lines).unwrap_or_else(|| "<anonymous>".into());
            let kind = match kind_str {
                "class_specifier" => OutlineKind::Class,
                "enum_specifier" => OutlineKind::Enum,
                _ => OutlineKind::Struct,
            };
            (kind, name, None)
        }

        // Interfaces & traits
        "interface_declaration"
        | "type_alias_declaration"
        | "trait_item"
        | "trait_declaration"
        | "trait_definition"
        | "protocol_declaration" => {
            let name = find_child_text(node, "name", lines).unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::Interface, name, None)
        }
        "type_item" | "type_definition" | "typealias_declaration" | "alias_declaration" => {
            // A C `typedef` names its type in a declarator, not a `name` field.
            let name = extract_definition_name(node, lines).unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::TypeAlias, name, None)
        }

        // Enums
        "enum_item" | "enum_declaration" | "enum_definition" => {
            let name = find_child_text(node, "name", lines).unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::Enum, name, None)
        }

        // Impl blocks (Rust)
        "impl_item" => {
            let name = find_child_text(node, "type", lines).unwrap_or_else(|| "<impl>".into());
            (OutlineKind::Module, format!("impl {name}"), None)
        }

        // Objects (Scala companion objects, singletons; Kotlin object declarations)
        "object_declaration" | "object_definition" => {
            let name = find_child_text(node, "name", lines)
                .or_else(|| find_child_text(node, "identifier", lines))
                .unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::Module, name, None)
        }

        // Constants and variables
        "const_item" | "const_declaration" | "static_item" => {
            let name = find_child_text(node, "name", lines)
                .or_else(|| first_identifier_text(node, lines))
                .unwrap_or_else(|| "<const>".into());
            (OutlineKind::Constant, name, None)
        }
        "val_definition" => {
            let name = first_identifier_text(node, lines).unwrap_or_else(|| "<val>".into());
            (OutlineKind::ImmutableVariable, name, None)
        }
        "lexical_declaration" | "variable_declaration" | "var_definition" => {
            let name = first_identifier_text(node, lines).unwrap_or_else(|| "<var>".into());
            (OutlineKind::Variable, name, None)
        }

        // Properties (C#, Swift, Kotlin)
        "property_declaration" | "protocol_property_declaration" => {
            let name = find_child_text(node, "name", lines)
                .or_else(|| first_identifier_text(node, lines))
                .unwrap_or_else(|| "<property>".into());
            let sig = first_line_signature(node, lines);
            (OutlineKind::Property, name, Some(sig))
        }

        // Imports — collect as a group
        "import_statement"
        | "import_from_statement"
        | "future_import_statement"
        | "import_declaration"
        | "import"
        | "use_declaration"
        | "namespace_use_declaration"
        | "use_item"
        | "using_directive" => {
            let text = node_text(node, lines);
            (OutlineKind::Import, text, None)
        }

        // Wrappers — `export` and Python decorators are modifiers on a wrapped
        // declaration, not peers of `function`/`class`/`const`. Recurse into
        // the inner declaration so the entry renders with its real kind. An
        // `export` shares the declaration's line, so the entry's range is
        // extended to cover it and still points at the whole statement. A
        // decorated entry keeps the `def`/`class` line as its first line, the
        // line every view prints for it. An `export` with no nameable declaration inside
        // (`export { … }`, `export * from …`, `export default <expr>`) falls
        // back to `OutlineKind::Export`. Without the unwrap,
        // `export_statement`'s `name` is the full source span (already starts
        // with `export `), and the renderer prepends the `Export` kind_label
        // `"export"` again — producing the doubled-keyword outline header
        // `export export async function foo(`.
        "export_statement" | "decorated_definition" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(mut inner) = node_to_entry(child, lines, lang, depth) {
                    if kind_str == "export_statement" {
                        inner.start_line = start_line;
                    }
                    return Some(inner);
                }
            }
            if kind_str == "decorated_definition" {
                return None;
            }
            // No nameable inner declaration; strip leading `export ` so the
            // rendered name doesn't duplicate the `kind_label`.
            let raw = node_text(node, lines);
            let name = raw
                .strip_prefix("export ")
                .map(str::to_string)
                .unwrap_or(raw);
            (OutlineKind::Export, name, None)
        }

        // Module declarations
        "mod_item"
        | "module"
        | "namespace_declaration"
        | "namespace_definition"
        | "file_scoped_namespace_declaration" => {
            let name = find_child_text(node, "name", lines).unwrap_or_else(|| "<module>".into());
            (OutlineKind::Module, name, None)
        }

        // Elixir: all definitions are `call` nodes distinguished by target identifier
        "call" if lang == Lang::Elixir => {
            return elixir_call_to_entry(node, lines, lang, depth);
        }

        // Elixir: @type, @typep, @opaque are unary_operator nodes
        "unary_operator" if lang == Lang::Elixir => {
            return elixir_attr_to_entry(node, lines);
        }

        // Bash: top-level variable assignments (`MY_VAR=value`, `ARR[0]=value`)
        "variable_assignment" if lang == Lang::Bash => {
            let name = assignment_name(node, lines).unwrap_or_else(|| "<var>".into());
            (OutlineKind::Variable, name, None)
        }

        // Bash: top-level `export` / `declare` / `readonly` declarations. The name
        // is the `name` of the inner variable_assignment (`export FOO=bar`) or a
        // bare variable_name child (`export FOO`). Function-local `local`
        // declarations are nested in function bodies, so walk_top_level never
        // reaches them here. Multi-variable declarations surface their first name.
        "declaration_command" if lang == Lang::Bash => {
            let mut cursor = node.walk();
            let name = node
                .children(&mut cursor)
                .find_map(|child| match child.kind() {
                    "variable_assignment" => assignment_name(child, lines),
                    "variable_name" => Some(node_text(child, lines)),
                    _ => None,
                })?;
            (OutlineKind::Variable, name, None)
        }

        _ => return None,
    };

    // Collect children for classes, impls, modules, traits/interfaces
    let is_namespace = NAMESPACE_KINDS.contains(&kind_str);
    let children = if matches!(
        kind,
        OutlineKind::Class | OutlineKind::Struct | OutlineKind::Module | OutlineKind::Interface
    ) && depth < 1
    {
        // Namespaces are transparent wrappers — don't consume a depth level,
        // so classes inside namespaces still collect their methods.
        let child_depth = if is_namespace { depth } else { depth + 1 };
        collect_children(node, lines, lang, child_depth)
    } else {
        Vec::new()
    };

    // Extract doc comment if present
    let doc = extract_doc(node, lines);

    Some(OutlineEntry {
        kind,
        name,
        start_line,
        end_line,
        signature,
        children,
        doc,
        local_name: None,
    })
}

/// Collect child entries from a class/struct/impl body.
fn collect_children(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: Lang,
    depth: usize,
) -> Vec<OutlineEntry> {
    let mut children = Vec::new();
    let mut cursor = node.walk();

    // Look for a body node first (C# uses `declaration_list` instead of
    // `*_body`/`*_block`, C/C++ classes and structs a `field_declaration_list`)
    let body = node.children(&mut cursor).find(|c| {
        let k = c.kind();
        k.contains("body")
            || k.contains("block")
            || k == "declaration_list"
            || k == "field_declaration_list"
    });

    let parent = body.unwrap_or(node);
    let mut cursor2 = parent.walk();

    for child in parent.children(&mut cursor2) {
        push_entries(child, lines, lang, depth, &mut children);
    }

    children
}

/// A function's signature: its name, parameters and return type, on one
/// line. A signature written across lines reads from its first line to
/// where its body starts, each line trimmed and joined, without its
/// comments.
fn extract_signature(node: tree_sitter::Node, lines: &[&str]) -> String {
    let header = header_lines(node, lines);
    if header.len() > 1 {
        return elide_middle(&join_signature_lines(&header));
    }
    first_line_signature(node, lines)
}

/// Longest signature an outline prints whole. A one-line signature past it
/// is cut at its end; a joined one loses its middle and keeps its end,
/// which holds the return type. fastapi's `APIRouter.get` header runs to
/// 14,000 characters of documented parameters.
const MAX_SIGNATURE: usize = 120;

/// `signature` with its middle replaced by `…` when it is longer than
/// `MAX_SIGNATURE`: three quarters of the room for its start, the rest for
/// its end.
fn elide_middle(signature: &str) -> String {
    if signature.len() <= MAX_SIGNATURE {
        return signature.to_string();
    }
    let head = signature.floor_char_boundary(MAX_SIGNATURE * 3 / 4);
    let tail = signature.ceil_char_boundary(signature.len() - MAX_SIGNATURE / 4);
    format!(
        "{} … {}",
        signature[..head].trim_end(),
        signature[tail..].trim_start()
    )
}

/// The non-blank lines of a function's header, trimmed and without its
/// comments: from the line it starts on to where its body starts, or to
/// its end when it has no body (an abstract or interface method). Empty
/// when the header fits on its first line.
fn header_lines(node: tree_sitter::Node, lines: &[&str]) -> Vec<String> {
    let start_row = node.start_position().row;
    let body = function_body(node);
    let end = match body {
        Some(body) => body.start_position(),
        // An Elixir `def f(x), do: …` is a call whose keyword body is
        // read by `first_line_signature`.
        None if node.kind() == "call" => return Vec::new(),
        None => node.end_position(),
    };
    if end.row == start_row {
        return Vec::new();
    }
    let mut comments = Vec::new();
    header_comments(node, body, &mut comments);
    let mut out = Vec::new();
    let last = end.row.min(lines.len().saturating_sub(1));
    for (row, &line) in lines.iter().enumerate().take(last + 1).skip(start_row) {
        let to = if row == end.row {
            end.column.min(line.len())
        } else {
            line.len()
        };
        let mut text = String::new();
        let mut from = 0;
        for comment in &comments {
            let (c_start, c_end) = (comment.start_position(), comment.end_position());
            if row < c_start.row || row > c_end.row {
                continue;
            }
            let cut_from = if row == c_start.row {
                c_start.column
            } else {
                0
            };
            let cut_to = if row == c_end.row {
                c_end.column
            } else {
                line.len()
            };
            text.push_str(line.get(from..cut_from.min(to)).unwrap_or(""));
            from = cut_to.max(from);
        }
        text.push_str(line.get(from.min(to)..to).unwrap_or(""));
        let text = text.trim();
        if !text.is_empty() {
            out.push(text.to_string());
        }
    }
    out
}

/// The comment nodes inside a function's header, in source order: every
/// descendant of `node` outside `body` whose kind is a comment.
fn header_comments<'t>(
    node: tree_sitter::Node<'t>,
    body: Option<tree_sitter::Node<'t>>,
    out: &mut Vec<tree_sitter::Node<'t>>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if Some(child) == body {
            continue;
        }
        if child.kind().contains("comment") {
            out.push(child);
        } else {
            header_comments(child, body, out);
        }
    }
}

/// The node a function's body starts at: its `body` field, or the child
/// that holds the body in grammars without one (Kotlin's `function_body`,
/// Elixir's `do_block`).
fn function_body(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    if let Some(body) = node.child_by_field_name("body") {
        return Some(body);
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|c| matches!(c.kind(), "function_body" | "do_block"));
    found
}

/// Header lines joined into one: no space inside brackets, and the
/// trailing comma a formatter puts before a closing bracket dropped, so a
/// signature reads the same however it is wrapped. A trailing `{`, the
/// Python `:` and a Rust `where` clause's last comma go too.
fn join_signature_lines(header: &[String]) -> String {
    let mut out = String::new();
    for part in header {
        let closes = part.starts_with([')', ']', '>', '}']);
        if closes && out.ends_with(',') {
            out.pop();
        }
        let opens = out.ends_with(['(', '[', '<', '{']);
        if !out.is_empty() && !closes && !opens {
            out.push(' ');
        }
        out.push_str(part);
    }
    let trimmed = out
        .trim_end()
        .trim_end_matches(';')
        .trim_end_matches('{')
        .trim_end();
    let trimmed = trimmed.strip_suffix(':').unwrap_or(trimmed);
    trimmed.trim_end().trim_end_matches(',').to_string()
}

/// The signature read off the node's first line alone.
fn first_line_signature(node: tree_sitter::Node, lines: &[&str]) -> String {
    let start_row = node.start_position().row;
    if start_row < lines.len() {
        let line = lines[start_row].trim();
        // Truncate at opening brace
        if let Some(pos) = line.find('{') {
            return line[..pos].trim().to_string();
        }
        if line.ends_with(':') {
            // Python — truncate at trailing colon (for `def foo(x: int):` etc.)
            if let Some(pos) = line.rfind(':') {
                return line[..pos].trim().to_string();
            }
        }
        // Elixir — truncate at ` do` (block form) or `, do:` (keyword form).
        // Safe for other languages: C/Java/Go/Rust hit the `{` branch above,
        // Python hits the `:` branch. Only Elixir uses ` do` as a block delimiter.
        if let Some(pos) = line.rfind(" do") {
            let after = &line[pos + 3..];
            if after.is_empty() || after.starts_with('\n') {
                return line[..pos].trim().to_string();
            }
        }
        if let Some(pos) = line.find(", do:") {
            return line[..pos].trim().to_string();
        }
        // Full first line, truncated
        if line.len() > MAX_SIGNATURE {
            format!("{}...", crate::types::truncate_str(line, MAX_SIGNATURE - 3))
        } else {
            line.to_string()
        }
    } else {
        String::new()
    }
}

/// Find a named child and return its text.
fn find_child_text(node: tree_sitter::Node, field: &str, lines: &[&str]) -> Option<String> {
    node.child_by_field_name(field).map(|n| node_text(n, lines))
}

/// Resolve the variable name from an assignment `name` field, unwrapping a
/// `subscript` (`ARR[0]=x`) to its base `variable_name` so the symbol
/// surfaces as `ARR`, not `ARR[0]`.
fn assignment_name(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let name = node.child_by_field_name("name")?;
    if name.kind() == "subscript" {
        let mut cursor = name.walk();
        let base = name
            .children(&mut cursor)
            .find(|c| c.kind() == "variable_name")
            .unwrap_or(name);
        Some(node_text(base, lines))
    } else {
        Some(node_text(name, lines))
    }
}

/// Get the text of a node, truncated to the first line.
fn node_text(node: tree_sitter::Node, lines: &[&str]) -> String {
    node_text_simple(node, lines, NodeTextMode::Truncated)
}

/// Find the first identifier-like child.
/// Recurses one level through declarators and `variable_declaration` nodes to find
/// the actual identifier inside wrapper nodes (e.g. Kotlin `property_declaration`
/// → `variable_declaration` → `simple_identifier`).
fn first_identifier_text(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind.contains("identifier") || kind.contains("name") {
            let text = node_text(child, lines);
            if !text.is_empty() {
                return Some(text);
            }
        }
        // Recurse one level through wrapper nodes (variable_declarator, variable_declaration)
        if kind.contains("declarator") || kind.contains("declaration") {
            let mut inner = child.walk();
            for grandchild in child.children(&mut inner) {
                if grandchild.kind().contains("identifier") {
                    let text = node_text(grandchild, lines);
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
        }
    }
    None
}

/// The doc comment above `node`: the comment right before it, and the
/// comments directly above that one, line after line. Line comments (`//`,
/// `///`, `#`) are one node per line, so a doc written in them is several
/// comments; reading only the nearest kept a doc's last line alone. Each
/// comment starts its own line, so one that trails the statement before
/// `node` is no doc. The comments above the nearest touch the one below
/// them: a blank line ends the doc. The nearest may sit a blank line above
/// `node`, as a `/** */` block often does in JavaScript.
fn extract_doc(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let is_comment = |n: tree_sitter::Node| {
        let kind = n.kind();
        kind.contains("comment") || kind.contains("doc")
    };
    let starts_its_line = |n: tree_sitter::Node| {
        let start = n.start_position();
        lines
            .get(start.row)
            .and_then(|line| line.get(..start.column))
            .is_some_and(|before| before.trim().is_empty())
    };
    // `above` ends on the line before `below` starts. A Rust line comment
    // ends after its newline, at column 0 of the next line.
    let touches = |above: tree_sitter::Node, below: tree_sitter::Node| {
        let end = above.end_position();
        let last_row = if end.column == 0 && end.row > above.start_position().row {
            end.row - 1
        } else {
            end.row
        };
        last_row + 1 == below.start_position().row
    };
    let nearest = node
        .prev_sibling()
        .filter(|p| is_comment(*p) && starts_its_line(*p))?;
    let mut block = vec![nearest];
    let mut top = nearest;
    while let Some(above) = top.prev_sibling() {
        if !(is_comment(above) && touches(above, top) && starts_its_line(above)) {
            break;
        }
        block.push(above);
        top = above;
    }
    let parts: Vec<String> = block
        .iter()
        .rev()
        .map(|c| clean_comment_text(&node_rows_text(*c, lines)))
        .filter(|text| !text.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Every line a node spans, joined with newlines. `node_text_simple` returns
/// only a node's first line, which is right for signatures and wrong for a
/// block comment that spans several.
fn node_rows_text(node: tree_sitter::Node, lines: &[&str]) -> String {
    let (start, end) = (node.start_position(), node.end_position());
    let last = end.row.min(lines.len().saturating_sub(1));
    let mut out = String::new();
    for (row, line) in lines.iter().enumerate().take(last + 1).skip(start.row) {
        let from = if row == start.row {
            start.column.min(line.len())
        } else {
            0
        };
        let to = if row == end.row {
            end.column.min(line.len())
        } else {
            line.len()
        };
        if row > start.row {
            out.push('\n');
        }
        out.push_str(&line[from..to]);
    }
    out
}

/// Comment text without its leaders, collapsed to one line. The outline
/// formatter prints a doc as `// <text>`, so a leader left in place shows up
/// doubled (`// // text`). Covers `///`, `//!`, `//`, `/* */`, `/** */` with
/// a `*` margin, and `#`.
fn clean_comment_text(text: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for line in text.lines() {
        let l = line
            .trim()
            .trim_end_matches("*/")
            .trim_start_matches("///")
            .trim_start_matches("//!")
            .trim_start_matches("/**")
            .trim_start_matches("/*")
            .trim_start_matches("//")
            .trim_start_matches('*')
            .trim_start_matches('#')
            .trim();
        if !l.is_empty() {
            parts.push(l);
        }
    }
    parts.join(" ")
}

// ---------------------------------------------------------------------------
// Go-specific outline helpers
// ---------------------------------------------------------------------------

/// Split a Go `type`, `const` or `var` declaration into one entry per spec.
///
/// The declaration carries no name of its own: each name sits on a spec
/// inside it. A grouped block (`const ( ... )`) holds one spec per member, so
/// every spec becomes its own entry with its own line range. `type` and
/// `const` groups hold their specs directly; `var` groups wrap them in a
/// `var_spec_list`. Returns `None` for any other node.
/// One `Export` entry per specifier of an export clause:
/// `export { readPlan as parsePlan, readNode }` and the re-export
/// `export { x } from "./y"`. Each is named as it is exported, carries the
/// name it refers to in `local_name` where the two differ, and spans the
/// clause's own lines. Its `signature` is the clause as written, on one
/// line, which is how the outline renders the clause. `None` for any other
/// node, including `export * from`, exports of a declaration and `export {}`.
fn export_clause_entries(node: tree_sitter::Node, lines: &[&str]) -> Option<Vec<OutlineEntry>> {
    if node.kind() != "export_statement" || node.child_by_field_name("declaration").is_some() {
        return None;
    }
    let mut cursor = node.walk();
    let clause = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "export_clause")?;

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    let signature = statement_on_one_line(node, lines);
    let doc = extract_doc(node, lines);

    let mut entries = Vec::new();
    let mut cursor = clause.walk();
    for specifier in clause.named_children(&mut cursor) {
        if specifier.kind() != "export_specifier" {
            continue;
        }
        let Some(local) = specifier.child_by_field_name("name") else {
            continue;
        };
        let local = node_text(local, lines);
        let exported = specifier
            .child_by_field_name("alias")
            .map(|alias| node_text(alias, lines));
        let (name, local_name) = match exported {
            Some(exported) if exported != local => (exported, Some(local)),
            _ => (local, None),
        };
        entries.push(OutlineEntry {
            kind: OutlineKind::Export,
            name,
            start_line,
            end_line,
            signature: Some(signature.clone()),
            children: Vec::new(),
            doc: doc.clone(),
            local_name,
        });
    }
    // `export {}` names nothing; it stays one entry, as written.
    (!entries.is_empty()).then_some(entries)
}

/// A node's full text with each line trimmed and the lines joined by one
/// space, so a statement written across lines reads as one.
fn statement_on_one_line(node: tree_sitter::Node, lines: &[&str]) -> String {
    let (start, end) = (node.start_position(), node.end_position());
    let rows = lines
        .iter()
        .enumerate()
        .skip(start.row)
        .take(end.row.saturating_sub(start.row) + 1);
    let mut parts = Vec::new();
    for (row, line) in rows {
        let from = if row == start.row {
            start.column.min(line.len())
        } else {
            0
        };
        let to = if row == end.row {
            end.column.min(line.len())
        } else {
            line.len()
        };
        let part = line.get(from..to).unwrap_or("").trim();
        if !part.is_empty() {
            parts.push(part);
        }
    }
    parts.join(" ")
}

fn go_declaration_entries(node: tree_sitter::Node, lines: &[&str]) -> Option<Vec<OutlineEntry>> {
    let container = match node.kind() {
        "type_declaration" | "const_declaration" => node,
        "var_declaration" => {
            let mut cursor = node.walk();
            let list = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "var_spec_list");
            list.unwrap_or(node)
        }
        _ => return None,
    };

    let mut cursor = container.walk();
    let grouped = container.children(&mut cursor).any(|c| c.kind() == "(");

    let mut entries = Vec::new();
    let mut cursor = container.walk();
    for spec in container.named_children(&mut cursor) {
        let kind = match spec.kind() {
            "const_spec" => OutlineKind::Constant,
            "var_spec" => OutlineKind::Variable,
            "type_alias" => OutlineKind::TypeAlias,
            "type_spec" => match spec.child_by_field_name("type").map(|t| t.kind()) {
                Some("struct_type") => OutlineKind::Struct,
                Some("interface_type") => OutlineKind::Interface,
                _ => OutlineKind::TypeAlias,
            },
            _ => continue,
        };

        // A spec may bind several names (`var a, b int`). The blank `_`
        // binds nothing, so a spec of only blanks is no definition.
        let mut name_cursor = spec.walk();
        let names: Vec<String> = spec
            .children_by_field_name("name", &mut name_cursor)
            .filter(tree_sitter::Node::is_named)
            .map(|n| node_text(n, lines))
            .filter(|n| n != "_")
            .collect();
        if names.is_empty() {
            continue;
        }

        // In a group, a member's comment sits directly above the member. A
        // single declaration's comment sits above its keyword.
        let doc = go_doc(if grouped { spec } else { node }, lines);

        entries.push(OutlineEntry {
            kind,
            name: names.join(", "),
            start_line: spec.start_position().row as u32 + 1,
            end_line: spec.end_position().row as u32 + 1,
            signature: None,
            children: Vec::new(),
            doc,
            local_name: None,
        });
    }

    Some(entries)
}

/// The doc comment directly above a Go spec or declaration. A comment that
/// trails code on its own line documents that code, so it is no doc for the
/// next member (`A = 1 // note` above `B = 2`).
fn go_doc(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let prev = node.prev_sibling()?;
    let start = prev.start_position();
    let before = lines.get(start.row)?.get(..start.column)?;
    if before.trim().is_empty() {
        extract_doc(node, lines)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Elixir-specific outline helpers
// ---------------------------------------------------------------------------

/// Elixir function-like definition keywords that produce `OutlineKind::Function`.
/// This is the subset of definition keywords handled uniformly (extract function
/// name from arguments). Container keywords (`defmodule`, `defprotocol`, `defimpl`,
/// `defstruct`, `defexception`) have their own match arms in `elixir_call_to_entry`.
/// See also `ELIXIR_DEFINITION_TARGETS` in `elixir.rs` for the complete set.
const ELIXIR_DEF_KEYWORDS: &[&str] = &[
    "def",
    "defp",
    "defmacro",
    "defmacrop",
    "defguard",
    "defguardp",
    "defdelegate",
];

/// Convert an Elixir `call` node to an outline entry.
///
/// In the Elixir tree-sitter grammar, `defmodule`, `def`, `defp`, `defstruct`,
/// etc. are all `call` nodes whose `target` field is an identifier like `"def"`.
fn elixir_call_to_entry(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: Lang,
    depth: usize,
) -> Option<OutlineEntry> {
    let target = node.child_by_field_name("target")?;
    let keyword = node_text(target, lines);
    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    let (kind, name, signature) = match keyword.as_str() {
        "defmodule" => {
            let name = elixir_first_arg_text(node, lines)?;
            (OutlineKind::Module, name, None)
        }
        kw if ELIXIR_DEF_KEYWORDS.contains(&kw) => {
            let name = elixir_func_name(node, lines)?;
            let sig = extract_signature(node, lines);
            (OutlineKind::Function, name, Some(sig))
        }
        "defstruct" | "defexception" => (OutlineKind::Struct, keyword.clone(), None),
        "defprotocol" => {
            let name = elixir_first_arg_text(node, lines)?;
            (OutlineKind::Interface, name, None)
        }
        "defimpl" => {
            let name = elixir_first_arg_text(node, lines)?;
            (OutlineKind::Module, format!("impl {name}"), None)
        }
        "use" | "import" | "alias" | "require" => {
            let text = node_text(node, lines);
            (OutlineKind::Import, text, None)
        }
        _ => return None,
    };

    // Collect children for modules, protocols, impls
    let children = if matches!(kind, OutlineKind::Module | OutlineKind::Interface) && depth < 1 {
        elixir_collect_children(node, lines, lang, depth + 1)
    } else {
        Vec::new()
    };

    // Extract @doc / @moduledoc from previous sibling
    let doc = elixir_extract_doc(node, lines);

    Some(OutlineEntry {
        kind,
        name,
        start_line,
        end_line,
        signature,
        children,
        doc,
        local_name: None,
    })
}

/// Convert an Elixir `unary_operator` node (`@type`, `@typep`, `@opaque`) to an outline entry.
fn elixir_attr_to_entry(node: tree_sitter::Node, lines: &[&str]) -> Option<OutlineEntry> {
    let operand = node.child_by_field_name("operand")?;
    if operand.kind() != "call" {
        return None;
    }
    let target = operand.child_by_field_name("target")?;
    let attr_name = node_text(target, lines);
    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;
    match attr_name.as_str() {
        "type" | "typep" | "opaque" => {
            let name = elixir_type_name(operand, lines)?;
            let sig = node_text(node, lines);
            Some(OutlineEntry {
                kind: OutlineKind::TypeAlias,
                name,
                start_line,
                end_line,
                signature: Some(sig),
                children: Vec::new(),
                doc: None,
                local_name: None,
            })
        }
        "callback" | "macrocallback" => {
            let name = elixir_callback_name(operand, lines)?;
            let sig = node_text(node, lines);
            Some(OutlineEntry {
                kind: OutlineKind::Function,
                name,
                start_line,
                end_line,
                signature: Some(sig),
                children: Vec::new(),
                doc: None,
                local_name: None,
            })
        }
        _ => None,
    }
}

/// Extract the first argument text from an Elixir call node.
/// For `defmodule Foo.Bar do ... end`, returns `"Foo.Bar"`.
fn elixir_first_arg_text(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let args = super::treesitter::elixir_arguments(node)?;
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if child.is_named() {
            return Some(node_text(child, lines));
        }
    }
    None
}

/// Extract function name from an Elixir `def`/`defp` call node.
///
/// For `def greet(name) do ... end`, the AST is:
///   call[target=def] → arguments → call[target=greet] → arguments → ...
/// For `def greet(name), do: ...` (keyword form), same structure.
fn elixir_func_name(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let args = super::treesitter::elixir_arguments(node)?;
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        return super::treesitter::elixir_extract_func_head_name(child, lines);
    }
    None
}

/// Extract type name from an Elixir `@type` call.
/// For `@type t :: %{...}`, the call operand is `type t :: %{...}`,
/// and we extract `t` from the first argument.
fn elixir_type_name(call: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let args = super::treesitter::elixir_arguments(call)?;
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        // `type t :: ...` → binary_operator with left=identifier
        if child.kind() == "binary_operator" {
            if let Some(left) = child.child_by_field_name("left") {
                // left may be a call like `t()` or an identifier `t`
                if left.kind() == "call" {
                    if let Some(target) = left.child_by_field_name("target") {
                        return Some(node_text(target, lines));
                    }
                }
                return Some(node_text(left, lines));
            }
        }
        // Bare identifier
        if child.kind() == "identifier" {
            return Some(node_text(child, lines));
        }
    }
    None
}

/// Extract callback name from an Elixir `@callback` call.
/// For `@callback handle_event(event :: term()) :: :ok`, the call operand is
/// `callback handle_event(...) :: :ok`. The arguments contain a `binary_operator`
/// with `::`, whose left side is a `call` with target = the callback name.
fn elixir_callback_name(call: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let args = super::treesitter::elixir_arguments(call)?;
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        if child.kind() == "binary_operator" {
            // `handle_event(...) :: return_type` → left is the function head
            if let Some(left) = child.child_by_field_name("left") {
                return super::treesitter::elixir_extract_func_head_name(left, lines);
            }
        }
        // Bare callback without return type spec (unlikely but handle it)
        return super::treesitter::elixir_extract_func_head_name(child, lines);
    }
    None
}

/// Collect child entries from an Elixir module/protocol/impl `do_block`.
///
/// This intentionally includes `use`/`alias`/`import`/`require` as import entries
/// inside module outlines. In Elixir these are structural — `use GenServer` injects
/// callbacks, `alias Foo.Bar` affects name resolution — so they provide useful
/// context alongside function definitions.
fn elixir_collect_children(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: Lang,
    depth: usize,
) -> Vec<OutlineEntry> {
    let mut children = Vec::new();
    let mut cursor = node.walk();

    // Find the do_block child
    let Some(do_block) = node.children(&mut cursor).find(|c| c.kind() == "do_block") else {
        return children;
    };

    let mut cursor2 = do_block.walk();
    for child in do_block.children(&mut cursor2) {
        if let Some(entry) = node_to_entry(child, lines, lang, depth) {
            children.push(entry);
        }
    }

    children
}

/// Extract @doc or @moduledoc text from the previous sibling of an Elixir definition.
///
/// In Elixir, `@doc "text"` is a `unary_operator` node. We check if the
/// previous sibling is such a node and extract the string content.
fn elixir_extract_doc(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let prev = node.prev_sibling()?;
    if prev.kind() != "unary_operator" {
        return None;
    }
    let operand = prev.child_by_field_name("operand")?;
    if operand.kind() != "call" {
        return None;
    }
    let target = operand.child_by_field_name("target")?;
    let attr = node_text(target, lines);
    if attr != "doc" && attr != "moduledoc" {
        return None;
    }
    // Get the doc argument — use tree-sitter node types to handle all forms:
    //   `@doc "text"`           → string node
    //   `@doc """heredoc"""`    → string node (multi-line)
    //   `@doc ~S"""sigil"""`    → sigil node
    //   `@doc ~s"""sigil"""`    → sigil node
    //   `@doc false`            → boolean node (suppress docs)
    let args = super::treesitter::elixir_arguments(operand)?;
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        match child.kind() {
            // `@doc false` suppresses documentation
            "boolean" => return None,
            // Regular string (`"text"`, `"""heredoc"""`) or sigil (`~S"""..."""`, `~s"""..."""`)
            "string" | "sigil" => {
                return elixir_extract_doc_string(child, lines);
            }
            _ => {}
        }
    }
    None
}

/// Extract the first meaningful line from an Elixir doc string or sigil node.
///
/// For single-line strings (`"text"`), returns the content without quotes.
/// For heredocs/sigils (`"""..."""`, `~S"""..."""`), returns the first
/// non-empty content line. Uses tree-sitter source lines rather than
/// fragile string trimming.
fn elixir_extract_doc_string(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let start_row = node.start_position().row;
    let end_row = node.end_position().row;

    if start_row == end_row {
        // Single-line: `"text"` or `~s"text"` — strip delimiters and sigil prefix
        let mut text = node_text(node, lines);
        // Strip sigil prefix (~s, ~S, etc.) if present
        if text.starts_with('~') && text.len() >= 2 {
            text = text[2..].to_string();
        }
        let trimmed = text.trim_matches('"').trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(trimmed.to_string());
    }

    // Multi-line (heredoc or sigil): scan interior lines for first non-empty content
    for row in (start_row + 1)..end_row {
        if row >= lines.len() {
            break;
        }
        let line = lines[row].trim();
        if !line.is_empty() && line != "\"\"\"" {
            return Some(line.to_string());
        }
    }
    None
}

/// Extract the source module name from an import statement text.
/// Handles: `use std::fs;` → `std::fs`, `import X from "react"` → `react`,
/// `from collections import X` → `collections`
///
/// The `lang` parameter is needed to disambiguate `use` (Rust path vs Elixir module)
/// and `import` (JS/TS `from` syntax vs Elixir/Python/Go bare module name).
pub fn extract_import_source(text: &str, lang: Option<crate::types::Lang>) -> String {
    let trimmed = text.trim().trim_end_matches(';');

    // Bash: `source ./lib.sh`, `. ./lib.sh`, or tab-separated variants
    if lang == Some(crate::types::Lang::Bash) {
        let after = trimmed
            .strip_prefix("source")
            .or_else(|| trimmed.strip_prefix('.'))
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map_or(trimmed, str::trim_start);
        // Skip variable-expanded paths (contain `$`)
        if after.contains('$') {
            return String::new();
        }
        return after.trim_matches(|c| c == '"' || c == '\'').to_string();
    }

    // Elixir: `use GenServer`, `import Kernel`, `alias Foo.Bar`, `require Logger`
    // Must be checked before the Rust `use` and JS `import` branches.
    if lang == Some(crate::types::Lang::Elixir) {
        for prefix in &["use ", "import ", "alias ", "require "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                return rest.split(',').next().unwrap_or(rest).trim().to_string();
            }
        }
        return trimmed.to_string();
    }

    // Rust: `use foo::bar` → `foo::bar`
    if let Some(rest) = trimmed.strip_prefix("use ") {
        return rest
            .split('{')
            .next()
            .unwrap_or(rest)
            .trim()
            .trim_end_matches("::")
            .to_string();
    }

    // JS/TS: `import ... from "source"` or `import "source"`
    if trimmed.starts_with("import") {
        if let Some(from_pos) = trimmed.find("from ") {
            let source = &trimmed[from_pos + 5..];
            return source
                .trim()
                .trim_matches(|c| c == '"' || c == '\'' || c == ';')
                .to_string();
        }
        // Direct import: `import "source"`
        let after = trimmed.strip_prefix("import ").unwrap_or("");
        return after
            .trim()
            .trim_matches(|c| c == '"' || c == '\'' || c == ';')
            .to_string();
    }

    // Python: `from module import ...` or `import module`
    if let Some(rest) = trimmed.strip_prefix("from ") {
        return rest.split_whitespace().next().unwrap_or("").to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("import ") {
        return rest.split_whitespace().next().unwrap_or("").to_string();
    }

    // C/C++: #include "file.h" or #include <header>
    if let Some(rest) = trimmed.strip_prefix("#include") {
        return rest.trim().to_string(); // preserves quotes/angles for external detection
    }

    // Go: `import "source"` — already handled above via "import"
    // Fallback: first meaningful token
    trimmed
        .split_whitespace()
        .last()
        .unwrap_or(trimmed)
        .to_string()
}

/// Get structured outline entries for file content.
#[must_use]
pub fn get_outline_entries(content: &str, lang: Lang) -> Vec<OutlineEntry> {
    let Some(ts_lang) = outline_language(lang) else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let lines: Vec<&str> = content.lines().collect();
    walk_top_level(tree.root_node(), &lines, lang)
}

#[cfg(test)]
mod markdown_helper_tests {
    use super::{heading_level, heading_text, parse_markdown};

    /// Walk the tree and collect every `atx_heading`/`setext_heading` node.
    fn collect_headings(tree: &tree_sitter::Tree) -> Vec<tree_sitter::Node<'_>> {
        let mut out = Vec::new();
        let mut cursor = tree.walk();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if matches!(node.kind(), "atx_heading" | "setext_heading") {
                out.push(node);
            }
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        out.sort_by_key(|n| n.start_position().row);
        out
    }

    #[test]
    fn parse_returns_block_tree_with_sections() {
        let src = "# Top\n\ncontent\n\n## Sub\n\nmore\n";
        let tree = parse_markdown(src).unwrap();
        let root = tree.root_node();
        assert_eq!(root.kind(), "document");
        // The document contains at least one section node.
        let mut cursor = root.walk();
        let has_section = root.children(&mut cursor).any(|c| c.kind() == "section");
        assert!(has_section, "expected document to contain section children");
    }

    #[test]
    fn fenced_code_blocks_do_not_emit_headings() {
        // The whole point: a `# foo` inside a fenced code block must NOT be
        // parsed as an atx_heading. The hand-rolled scanners had to track
        // fence state manually; the AST does this for free.
        let src = "# Real\n\n```python\n# fake heading\nprint('x')\n```\n\n## Also Real\n";
        let tree = parse_markdown(src).unwrap();
        let headings = collect_headings(&tree);
        let lines: Vec<&str> = src.lines().collect();
        let texts: Vec<String> = headings.iter().map(|n| heading_text(*n, &lines)).collect();
        assert_eq!(texts, vec!["Real".to_string(), "Also Real".to_string()]);
    }

    #[test]
    fn tilde_fences_are_recognised() {
        let src = "# Real\n\n~~~\n# inside tilde fence\n~~~\n\n## Other\n";
        let tree = parse_markdown(src).unwrap();
        let lines: Vec<&str> = src.lines().collect();
        let headings = collect_headings(&tree);
        let texts: Vec<String> = headings.iter().map(|n| heading_text(*n, &lines)).collect();
        assert_eq!(texts, vec!["Real".to_string(), "Other".to_string()]);
    }

    #[test]
    fn level_extraction_covers_h1_through_h6() {
        let src = "# A\n\n## B\n\n### C\n\n#### D\n\n##### E\n\n###### F\n";
        let tree = parse_markdown(src).unwrap();
        let headings = collect_headings(&tree);
        let levels: Vec<u8> = headings.iter().filter_map(|n| heading_level(*n)).collect();
        assert_eq!(levels, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn trailing_atx_close_hashes_are_stripped() {
        let src = "## Foo ##\n";
        let tree = parse_markdown(src).unwrap();
        let lines: Vec<&str> = src.lines().collect();
        let headings = collect_headings(&tree);
        assert_eq!(heading_text(headings[0], &lines), "Foo");
    }
}

#[cfg(test)]
mod bash_outline_tests {
    use super::{extract_import_source, get_outline_entries};
    use crate::search::callees::extract_callee_names;
    use crate::types::{Lang, OutlineKind};

    // Fixture covering both function syntaxes, top-level vars, and a nested local.
    const BASH_FIXTURE: &str = r#"MY_CONST=hello
DEBUG_MODE=0

greet() { echo "hi $1"; }

function cleanup {
    rm -f /tmp/x
}

main() {
    greet world
    cleanup
    local y=1
}
"#;

    #[test]
    fn bash_outline_functions_and_vars() {
        let entries = get_outline_entries(BASH_FIXTURE, Lang::Bash);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        // All three functions must appear
        assert!(
            names.contains(&"greet"),
            "expected greet in outline, got: {names:?}"
        );
        assert!(
            names.contains(&"cleanup"),
            "expected cleanup in outline, got: {names:?}"
        );
        assert!(
            names.contains(&"main"),
            "expected main in outline, got: {names:?}"
        );

        // Top-level variables must appear
        assert!(
            names.contains(&"MY_CONST"),
            "expected MY_CONST in outline, got: {names:?}"
        );
        assert!(
            names.contains(&"DEBUG_MODE"),
            "expected DEBUG_MODE in outline, got: {names:?}"
        );

        // Functions must have Function kind
        for fname in &["greet", "cleanup", "main"] {
            let entry = entries.iter().find(|e| e.name == *fname).unwrap();
            assert_eq!(
                entry.kind,
                OutlineKind::Function,
                "{fname} should be OutlineKind::Function"
            );
        }

        // Variables must have Variable kind
        for vname in &["MY_CONST", "DEBUG_MODE"] {
            let entry = entries.iter().find(|e| e.name == *vname).unwrap();
            assert_eq!(
                entry.kind,
                OutlineKind::Variable,
                "{vname} should be OutlineKind::Variable"
            );
        }

        // Nested `local y=1` must NOT appear at the top level
        assert!(
            !names.contains(&"y"),
            "nested local 'y' must not appear in top-level outline, got: {names:?}"
        );
    }

    #[test]
    fn bash_callee_names_for_main() {
        // Derive main's range from the outline so the test can't silently drift
        // if the fixture is edited.
        let main = get_outline_entries(BASH_FIXTURE, Lang::Bash)
            .into_iter()
            .find(|e| e.name == "main")
            .expect("main must be in the outline");
        let names = extract_callee_names(
            BASH_FIXTURE,
            Lang::Bash,
            Some((main.start_line, main.end_line)),
        );

        assert!(
            names.contains(&"greet".to_string()),
            "expected greet as callee, got: {names:?}"
        );
        assert!(
            names.contains(&"cleanup".to_string()),
            "expected cleanup as callee, got: {names:?}"
        );
        // echo is called inside greet, outside main's range, so it is absent here.
    }

    #[test]
    fn bash_outline_surfaces_declarations_and_hyphenated_names() {
        // export/declare/readonly declarations must surface (the common config
        // pattern), and hyphenated function names must be captured whole.
        let src = "export E_VAR=1\n\
                   declare -r D_VAR=2\n\
                   readonly R_VAR=3\n\
                   deploy-app() { :; }\n";
        let entries = get_outline_entries(src, Lang::Bash);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        for v in ["E_VAR", "D_VAR", "R_VAR"] {
            let e = entries
                .iter()
                .find(|e| e.name == v)
                .unwrap_or_else(|| panic!("{v} should be outlined, got: {names:?}"));
            assert_eq!(e.kind, OutlineKind::Variable, "{v} should be a Variable");
        }
        let dep = entries
            .iter()
            .find(|e| e.name == "deploy-app")
            .unwrap_or_else(|| panic!("deploy-app should be outlined whole, got: {names:?}"));
        assert_eq!(dep.kind, OutlineKind::Function);
    }

    #[test]
    fn bash_extract_import_source_source_keyword() {
        let line = "source ./lib/utils.sh";
        let result = extract_import_source(line, Some(Lang::Bash));
        assert_eq!(result, "./lib/utils.sh");
    }

    #[test]
    fn bash_extract_import_source_dot_keyword() {
        let line = ". ./config.sh";
        let result = extract_import_source(line, Some(Lang::Bash));
        assert_eq!(result, "./config.sh");
    }

    #[test]
    fn bash_extract_import_source_quoted() {
        let line = r#"source "./lib/helpers.sh""#;
        let result = extract_import_source(line, Some(Lang::Bash));
        assert_eq!(result, "./lib/helpers.sh");
    }

    #[test]
    fn bash_extract_import_source_variable_expanded_returns_empty() {
        let line = r#"source "$DIR/lib.sh""#;
        let result = extract_import_source(line, Some(Lang::Bash));
        assert!(
            result.is_empty(),
            "variable-expanded source should return empty, got: {result:?}"
        );
    }

    #[test]
    fn bash_subscript_assignment_surfaces_base_name() {
        // `ARR[0]=hello` should appear as `ARR` (Variable), not `ARR[0]`.
        let entries = get_outline_entries("ARR[0]=hello\n", Lang::Bash);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"ARR"),
            "expected ARR in outline, got: {names:?}"
        );
        assert!(
            !names.contains(&"ARR[0]"),
            "ARR[0] must not appear verbatim in outline, got: {names:?}"
        );
        let entry = entries.iter().find(|e| e.name == "ARR").unwrap();
        assert_eq!(
            entry.kind,
            OutlineKind::Variable,
            "ARR should be OutlineKind::Variable"
        );
    }

    #[test]
    fn bash_extract_import_source_tab_separated() {
        // `source\t./lib.sh` (tab separator) must be parsed correctly.
        let result = extract_import_source("source\t./lib/utils.sh", Some(Lang::Bash));
        assert_eq!(result, "./lib/utils.sh");
    }
}

#[cfg(test)]
mod go_outline_tests {
    use super::get_outline_entries;
    use crate::lang::go::GO_DECLARATIONS_FIXTURE;
    use crate::types::{Lang, OutlineKind};

    /// `(name, kind, start_line, end_line)` for each top-level entry.
    fn summary(src: &str) -> Vec<(String, OutlineKind, u32, u32)> {
        get_outline_entries(src, Lang::Go)
            .into_iter()
            .map(|e| (e.name, e.kind, e.start_line, e.end_line))
            .collect()
    }

    #[test]
    fn go_outline_lists_type_const_and_var_declarations() {
        // Each member of the grouped `const ( ... )` block is its own entry on
        // its own line (4 and 5), not one entry for the block on line 3.
        let expected = vec![
            ("StatusActive".to_string(), OutlineKind::Constant, 4, 4),
            ("StatusInactive".to_string(), OutlineKind::Constant, 5, 5),
            ("Version".to_string(), OutlineKind::Constant, 7, 7),
            ("GlobalCounter".to_string(), OutlineKind::Variable, 8, 8),
            ("Server".to_string(), OutlineKind::Struct, 9, 9),
            ("NewServer".to_string(), OutlineKind::Function, 10, 10),
        ];
        assert_eq!(summary(GO_DECLARATIONS_FIXTURE), expected);
    }

    #[test]
    fn go_outline_splits_grouped_type_and_var_blocks() {
        // A `type ( ... )` group holds its specs directly; a `var ( ... )`
        // group wraps them in a spec list. Both yield one entry per spec. A
        // spec that binds several names lists them all; a blank `_` binds
        // nothing, so it is left out. Methods keep working.
        let src = "package p

type (
\tID int
\tAlias = string
\tHandler interface {
\t\tServe()
\t}
)

var (
\tx int
\ty, z = 1, 2
\t_ = x
)

func (s *Server) Run() {}
";
        let expected = vec![
            ("ID".to_string(), OutlineKind::TypeAlias, 4, 4),
            ("Alias".to_string(), OutlineKind::TypeAlias, 5, 5),
            ("Handler".to_string(), OutlineKind::Interface, 6, 8),
            ("x".to_string(), OutlineKind::Variable, 12, 12),
            ("y, z".to_string(), OutlineKind::Variable, 13, 13),
            ("Run".to_string(), OutlineKind::Function, 17, 17),
        ];
        assert_eq!(summary(src), expected);
    }

    #[test]
    fn go_outline_attaches_doc_comments_to_specs() {
        // A grouped spec carries the comment directly above it. A single
        // declaration carries the comment above its keyword. A comment that
        // trails a member's line belongs to that member, not the next one.
        let src = "package p

// Version is the release.
const Version = \"1\"

const (
\t// First is documented.
\tFirst = 1 // trails First
\tSecond = 2
)
";
        let entries = get_outline_entries(src, Lang::Go);
        let doc = |name: &str| {
            entries
                .iter()
                .find(|e| e.name == name)
                .unwrap_or_else(|| panic!("{name} missing from outline"))
                .doc
                .clone()
        };
        assert!(
            doc("Version").is_some_and(|d| d.contains("Version is the release.")),
            "Version doc: {:?}",
            doc("Version")
        );
        assert!(
            doc("First").is_some_and(|d| d.contains("First is documented.")),
            "First doc: {:?}",
            doc("First")
        );
        assert_eq!(doc("Second"), None, "Second has no comment of its own");
    }
}

#[cfg(test)]
mod doc_comment_tests {
    use super::get_outline_entries;
    use crate::types::Lang;

    fn first_doc(src: &str, lang: Lang) -> Option<String> {
        get_outline_entries(src, lang)
            .into_iter()
            .next()
            .and_then(|e| e.doc)
    }

    /// A plain `//` comment is a doc comment too. It used to keep its slashes,
    /// and the outline formatter then added its own, printing `// // text`.
    #[test]
    fn doc_strips_plain_line_comment_markers() {
        assert_eq!(
            first_doc(
                "package p\n\n// Serve handles requests.\nfunc Serve() {}\n",
                Lang::Go
            ),
            Some("Serve handles requests.".to_string())
        );
        assert_eq!(
            first_doc("// plain note\nfn f() {}\n", Lang::Rust),
            Some("plain note".to_string())
        );
        assert_eq!(
            first_doc("/// doc note\nfn f() {}\n", Lang::Rust),
            Some("doc note".to_string())
        );
    }

    /// Block comments lose their delimiters, and a multi-line block collapses
    /// to one line with the leading `*` of each line removed.
    #[test]
    fn doc_strips_block_comment_delimiters() {
        assert_eq!(
            first_doc("/* block note */\nint f(void) { return 0; }\n", Lang::C),
            Some("block note".to_string())
        );
        let js = "/**\n * Adds two numbers.\n * @param a first\n */\nfunction add(a, b) { return a + b; }\n";
        let doc = first_doc(js, Lang::JavaScript).expect("jsdoc");
        assert!(doc.starts_with("Adds two numbers."), "got: {doc:?}");
        assert!(!doc.contains('*'), "delimiters must be gone: {doc:?}");
        assert!(!doc.contains('\n'), "must be one line: {doc:?}");
    }

    /// A doc written in line comments is one comment node per line. Reading
    /// only the nearest node kept the doc's last line: `Second sentence.`
    /// A blank line or a comment that trails code ends the doc.
    #[test]
    fn a_doc_of_line_comments_keeps_every_line() {
        assert_eq!(
            first_doc(
                "package p\n\n// Public does a thing.\n// Second sentence.\nfunc Public() {}\n",
                Lang::Go
            ),
            Some("Public does a thing. Second sentence.".to_string())
        );
        assert_eq!(
            first_doc("/// First line.\n/// Second line.\nfn f() {}\n", Lang::Rust),
            Some("First line. Second line.".to_string())
        );
        assert_eq!(
            first_doc(
                "# Section heading\n\n# Doc of f.\n# More.\ndef f():\n    pass\n",
                Lang::Python
            ),
            Some("Doc of f. More.".to_string())
        );
        let grouped =
            "package p\n\nconst (\n\tA = 1 // trails A\n\t// B is documented.\n\tB = 2\n)\n";
        let entries = get_outline_entries(grouped, Lang::Go);
        let b = entries.iter().find(|e| e.name == "B").expect("B");
        assert_eq!(b.doc.as_deref(), Some("B is documented."));
    }

    /// A comment that trails the statement before a definition documents
    /// that statement. A comment on its own lines a blank line above stays
    /// the doc, the way a `/** */` block above a function often sits.
    #[test]
    fn a_comment_trailing_code_is_no_doc() {
        let entries = get_outline_entries(
            "TIMEOUT = 3  # seconds\ndef fetch():\n    return TIMEOUT\n",
            Lang::Python,
        );
        assert_eq!(
            entries.iter().find(|e| e.name == "fetch").unwrap().doc,
            None
        );
        assert_eq!(
            first_doc("int x = 1; /* x */\nint f(void) { return 0; }\n", Lang::C),
            None
        );
        assert_eq!(
            first_doc(
                "/**\n * Create an app.\n */\n\nfunction createApp() {}\n",
                Lang::JavaScript
            ),
            Some("Create an app.".to_string())
        );
    }
}

#[cfg(test)]
mod signature_tests {
    use super::get_outline_entries;
    use crate::types::Lang;
    use std::fmt::Write as _;

    fn signature_of(src: &str, lang: Lang, name: &str) -> Option<String> {
        fn find(entries: &[crate::types::OutlineEntry], name: &str) -> Option<String> {
            entries.iter().find_map(|e| {
                if e.name == name {
                    e.signature.clone()
                } else {
                    find(&e.children, name)
                }
            })
        }
        find(&get_outline_entries(src, lang), name)
    }

    /// A signature written across lines reads to where the body starts.
    /// Read off the first line alone, it was `pub fn multiline_handler(`.
    #[test]
    fn a_multiline_signature_is_read_whole() {
        let rust = "pub fn multiline_handler(\n    request: &str,\n    retries: u8,\n) -> Result<usize, &'static str> {\n    Ok(1)\n}\n";
        assert_eq!(
            signature_of(rust, Lang::Rust, "multiline_handler").as_deref(),
            Some("pub fn multiline_handler(request: &str, retries: u8) -> Result<usize, &'static str>")
        );
        let where_clause = "fn f<T>(x: T) -> T\nwhere\n    T: Clone,\n{\n    x\n}\n";
        assert_eq!(
            signature_of(where_clause, Lang::Rust, "f").as_deref(),
            Some("fn f<T>(x: T) -> T where T: Clone")
        );
        let python = "def handler(\n    request,\n    retries: int = 3,\n) -> int:\n    return 1\n";
        assert_eq!(
            signature_of(python, Lang::Python, "handler").as_deref(),
            Some("def handler(request, retries: int = 3) -> int")
        );
        let go =
            "package p\n\nfunc Serve(\n\taddr string,\n\tport int,\n) error {\n\treturn nil\n}\n";
        assert_eq!(
            signature_of(go, Lang::Go, "Serve").as_deref(),
            Some("func Serve(addr string, port int) error")
        );
        let ts = "export async function load(\n  path: string,\n  opts: Options,\n): Promise<void> {\n}\n";
        assert_eq!(
            signature_of(ts, Lang::TypeScript, "load").as_deref(),
            Some("export async function load(path: string, opts: Options): Promise<void>")
        );
        let commented =
            "def handler(  # noqa: N802\n    request,  # the request\n) -> int:\n    return 1\n";
        assert_eq!(
            signature_of(commented, Lang::Python, "handler").as_deref(),
            Some("def handler(request) -> int")
        );
        let java = "interface Store {\n  void save(\n      String key,\n      byte[] value);\n}\n";
        assert_eq!(
            signature_of(java, Lang::Java, "save").as_deref(),
            Some("void save(String key, byte[] value)")
        );
    }

    /// A wrapped signature longer than `MAX_SIGNATURE` keeps its
    /// start and its end, with the return type. Whole, fastapi's
    /// `APIRouter.get` printed 14,000 characters in every outline.
    #[test]
    fn a_very_long_signature_keeps_its_start_and_return_type() {
        let mut params = String::new();
        for i in 0..40 {
            let _ = writeln!(params, "    argument_{i}: str = None,");
        }
        let python = format!("def handler(\n{params}) -> Response:\n    return 1\n");
        let sig = signature_of(&python, Lang::Python, "handler").unwrap();
        assert!(sig.len() <= super::MAX_SIGNATURE + 8, "{sig}");
        assert!(
            sig.starts_with("def handler(argument_0: str = None, "),
            "{sig}"
        );
        assert!(sig.contains(" … "), "{sig}");
        assert!(sig.ends_with("_39: str = None) -> Response"), "{sig}");
    }

    /// A signature that fits on its line reads as it did: to its brace,
    /// or the whole line.
    #[test]
    fn a_one_line_signature_reads_as_before() {
        assert_eq!(
            signature_of("fn f(a: u8) -> u8 { a }\n", Lang::Rust, "f").as_deref(),
            Some("fn f(a: u8) -> u8")
        );
        assert_eq!(
            signature_of("def f(x):\n    return x\n", Lang::Python, "f").as_deref(),
            Some("def f(x)")
        );
        assert_eq!(
            signature_of("def f(x), do: x\n", Lang::Elixir, "f").as_deref(),
            Some("def f(x)")
        );
    }
}

#[cfg(test)]
mod outline_tree_tests {
    use super::get_outline_entries;
    use crate::lang::c::C_DEFINITIONS_FIXTURE;
    use crate::lang::cpp::CPP_DEFINITIONS_FIXTURE;
    use crate::types::{Lang, OutlineEntry, OutlineKind};

    /// `(name, kind, start_line, end_line, children)` for an entry tree.
    #[derive(Debug, PartialEq)]
    struct Node(String, OutlineKind, u32, u32, Vec<Self>);

    fn tree(entries: Vec<OutlineEntry>) -> Vec<Node> {
        entries
            .into_iter()
            .map(|e| Node(e.name, e.kind, e.start_line, e.end_line, tree(e.children)))
            .collect()
    }

    fn leaf(name: &str, kind: OutlineKind, start: u32, end: u32) -> Node {
        Node(name.to_string(), kind, start, end, Vec::new())
    }

    #[test]
    fn c_outline_names_functions_and_types() {
        // Functions carry their bare name whatever wraps it, the include
        // guard does not hide the types inside it, and the forward
        // `struct point;` and the prototype are not entries.
        use OutlineKind::{Enum, Function, Struct, TypeAlias};
        let expected = vec![
            leaf("point", Struct, 4, 4),
            leaf("number", Struct, 5, 5),
            leaf("color", Enum, 6, 6),
            leaf("point_t", TypeAlias, 7, 7),
            leaf("compare_fn", TypeAlias, 8, 8),
            leaf("add", Function, 11, 13),
            leaf("make", Function, 14, 14),
            leaf("label", Function, 15, 15),
            leaf("pick", Function, 16, 16),
            leaf("origin", Function, 17, 17),
            leaf("main", Function, 18, 18),
        ];
        assert_eq!(
            tree(get_outline_entries(C_DEFINITIONS_FIXTURE, Lang::C)),
            expected
        );
    }

    #[test]
    fn cpp_outline_names_methods_namespaces_and_templates() {
        // Classes list their in-class method definitions, a namespace lists
        // what it holds, and templates and `extern "C"` blocks show the
        // definition they wrap. Declarations without a body are not entries.
        use OutlineKind::{Class, Enum, Function, Module, Struct, TypeAlias};
        let expected = vec![
            leaf("ref", Function, 1, 1),
            Node(
                "Shape".into(),
                Class,
                2,
                9,
                vec![
                    leaf("~Shape", Function, 5, 5),
                    leaf("area", Function, 6, 6),
                    leaf("operator==", Function, 7, 7),
                ],
            ),
            leaf("Shape", Function, 10, 10),
            leaf("sides", Function, 11, 11),
            Node(
                "geo".into(),
                Module,
                13,
                20,
                vec![
                    leaf("detail", Module, 14, 14),
                    leaf("helper", Function, 15, 15),
                    leaf("biggest", Function, 16, 16),
                    Node(
                        "Box".into(),
                        Struct,
                        17,
                        19,
                        vec![leaf("volume", Function, 18, 18)],
                    ),
                ],
            ),
            leaf("biggest", Function, 21, 21),
            leaf("c_entry", Function, 22, 22),
            leaf("Size", TypeAlias, 23, 23),
            leaf("Mode", Enum, 24, 24),
        ];
        assert_eq!(
            tree(get_outline_entries(CPP_DEFINITIONS_FIXTURE, Lang::Cpp)),
            expected
        );
    }

    #[test]
    fn ruby_outline_lists_classes_modules_and_methods() {
        // #226: a class's instance methods and a module's `def self.`
        // functions are entries, and a class inside a module collects its
        // methods like a top-level class.
        use OutlineKind::{Class, Function, Module};
        let src = "module Shop
  class Store
    def save
      1
    end

    def self.build
      new
    end
  end
end

module Util
  def self.slugify(s)
    s
  end

  def self.titleize(s)
    s
  end
end
";
        let expected = vec![
            Node(
                "Shop".into(),
                Module,
                1,
                11,
                vec![Node(
                    "Store".into(),
                    Class,
                    2,
                    10,
                    vec![leaf("save", Function, 3, 5), leaf("build", Function, 7, 9)],
                )],
            ),
            Node(
                "Util".into(),
                Module,
                13,
                21,
                vec![
                    leaf("slugify", Function, 14, 16),
                    leaf("titleize", Function, 18, 20),
                ],
            ),
        ];
        assert_eq!(tree(get_outline_entries(src, Lang::Ruby)), expected);
    }

    #[test]
    fn python_outline_lists_decorated_definitions_from_their_def_line() {
        use OutlineKind::{Class, Function};
        let src = "@decorator
def top():
    pass

class Service:
    @property
    def prop(self):
        pass

    def plain(self):
        pass

@dataclass
class Point:
    @staticmethod
    def origin():
        pass
";
        let expected = vec![
            leaf("top", Function, 2, 3),
            Node(
                "Service".into(),
                Class,
                5,
                11,
                vec![
                    leaf("prop", Function, 7, 8),
                    leaf("plain", Function, 10, 11),
                ],
            ),
            Node(
                "Point".into(),
                Class,
                14,
                17,
                vec![leaf("origin", Function, 16, 17)],
            ),
        ];
        assert_eq!(tree(get_outline_entries(src, Lang::Python)), expected);
    }

    #[test]
    fn typescript_outline_lists_abstract_classes_and_their_members() {
        use OutlineKind::{Class, Function};
        let src = "abstract class Shape {
  abstract area(): number;
  describe() {}
}
export abstract class Base {
  run() {}
}
export default abstract class Fallback {}
";
        let expected = vec![
            Node(
                "Shape".into(),
                Class,
                1,
                4,
                vec![
                    leaf("area", Function, 2, 2),
                    leaf("describe", Function, 3, 3),
                ],
            ),
            Node(
                "Base".into(),
                Class,
                5,
                7,
                vec![leaf("run", Function, 6, 6)],
            ),
            leaf("Fallback", Class, 8, 8),
        ];
        for lang in [Lang::TypeScript, Lang::Tsx] {
            assert_eq!(tree(get_outline_entries(src, lang)), expected, "{lang:?}");
        }
    }

    /// `extern "C"` written before one function, with no braces, wraps that
    /// function alone: the function is the entry, as with a block.
    #[test]
    fn an_extern_c_function_without_braces_is_an_entry() {
        use OutlineKind::Function;
        let src = "extern \"C\" int f(void) {\n  return 1;\n}\n\nint g(void) {\n  return 2;\n}\n";
        let expected = vec![leaf("f", Function, 1, 3), leaf("g", Function, 5, 7)];
        assert_eq!(tree(get_outline_entries(src, Lang::Cpp)), expected);
    }

    /// A Ruby class with no body has no children: its superclass is not an
    /// anonymous class inside it.
    #[test]
    fn a_ruby_class_without_a_body_has_no_children() {
        use OutlineKind::Class;
        let src = "class X < Y; end\n\nclass Z < Y\nend\n";
        let expected = vec![leaf("X", Class, 1, 1), leaf("Z", Class, 3, 4)];
        assert_eq!(tree(get_outline_entries(src, Lang::Ruby)), expected);
    }

    /// A generator function is an outline function, as a plain one is.
    #[test]
    fn a_generator_function_is_an_entry() {
        use OutlineKind::Function;
        let src = "function* gen() {\n  yield 1;\n}\nexport function* more() {\n  yield 2;\n}\n";
        let expected = vec![leaf("gen", Function, 1, 3), leaf("more", Function, 4, 6)];
        for lang in [Lang::JavaScript, Lang::TypeScript] {
            assert_eq!(tree(get_outline_entries(src, lang)), expected, "{lang:?}");
        }
    }
}
