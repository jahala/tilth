use std::path::Path;

use crate::types::QueryType;

/// Classify a query string into a `QueryType` by byte-pattern matching.
/// No regex engine — `matches!` compiles to a jump table.
pub fn classify(query: &str, scope: &Path) -> QueryType {
    // 0. Slash-wrapped regex — /pattern/ → regex content search.
    //    Must come before glob check: regex metacharacters ([, {, *) overlap with glob syntax.
    //    Only if the inner pattern contains regex metacharacters — otherwise /src/ would be
    //    misclassified as regex instead of a path.
    if query.len() >= 3 && query.starts_with('/') && query.ends_with('/') {
        let pattern = &query[1..query.len() - 1];
        if !pattern.is_empty() && has_regex_metachar(pattern) {
            return QueryType::Regex(pattern.into());
        }
    }

    // 1. Glob — check first because globs can contain path separators.
    //    But only if no spaces: real globs don't have spaces, content like "import { X }" does.
    if !query.contains(' ')
        && query
            .bytes()
            .any(|b| matches!(b, b'*' | b'?' | b'{' | b'['))
    {
        return QueryType::Glob(query.into());
    }

    // 2. File path — contains separator or starts with ./ ../
    //    But only if no spaces around the separator ("TODO: fix this/that" is content, not a path)
    if (query.starts_with("./") || query.starts_with("../"))
        || (query.contains('/') && !query.contains(' '))
    {
        let resolved = scope.join(query);
        return match resolved.try_exists() {
            Ok(true) => QueryType::FilePath(resolved),
            _ => QueryType::Fallthrough(query.into()),
        };
    }

    // 3. Starts with . — could be dotfile (.gitignore) or relative path
    if query.starts_with('.') {
        let resolved = scope.join(query);
        if resolved.try_exists().unwrap_or(false) {
            return QueryType::FilePath(resolved);
        }
    }

    // 4. Pure numeric — always content search (HTTP codes, error numbers)
    if query.bytes().all(|b| b.is_ascii_digit()) {
        return QueryType::Content(query.into());
    }

    // 5. Bare filename — only check filesystem for queries that look like filenames
    //    (have an extension or match known extensionless names like README, Makefile, etc.)
    if looks_like_filename(query) {
        let resolved = scope.join(query);
        if resolved.try_exists().unwrap_or(false) {
            return QueryType::FilePath(resolved);
        }
        // Not found at scope root — glob fallback, but only if this isn't a dotted symbol
        // like "Auth.validate" which should fall through to identifier/symbol check
        if !is_dotted_symbol(query) {
            return QueryType::Glob(format!("**/{query}"));
        }
    }

    // 6. Identifier — no whitespace, starts with letter/underscore/$/@
    if is_identifier(query) {
        // Sub-classify: exact symbol vs concept
        if looks_like_exact_symbol(query) {
            return QueryType::Symbol(query.into());
        }
        return QueryType::Concept(query.into());
    }

    // 7. OR-pattern — "Foo|Bar|Baz" with no spaces, all parts valid identifiers.
    //    Common developer intent: multi-symbol grep (rg "A|B|C" equivalent).
    //    Wrap in word boundaries so "Foo|Bar" doesn't match "Foobar"/"Barbarian".
    if !query.contains(' ') && query.contains('|') {
        let parts: Vec<&str> = query.split('|').filter(|s| !s.is_empty()).collect();
        if parts.len() >= 2 && parts.iter().all(|p| is_identifier(p)) {
            return QueryType::Regex(format!(r"\b({query})\b"));
        }
    }

    // 8. Multi-word — could be concept phrase ("cli mode", "search flow")
    if query.contains(' ') && query.split_whitespace().count() <= 4 {
        let words: Vec<&str> = query.split_whitespace().collect();
        let all_simple = words.iter().all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
        if all_simple {
            return QueryType::Concept(query.into());
        }
    }

    // 9. Everything else
    QueryType::Content(query.into())
}

/// Does this single-token query look like an exact symbol name?
///
/// Heuristics (all generic, no domain knowledge):
/// - `PascalCase` (starts uppercase): `SearchResult`, `MapModel`, `AuthService`
/// - Contains `::` or `.`: `std::path`, `Auth.validate`
/// - `snake_case` with underscore: `handle_auth`, `is_test_file`
/// - Has mixed case after first char: `handleAuth`, `getElementById`
/// - Starts with `$` or `@`: `$ref`, `@decorator`
fn looks_like_exact_symbol(query: &str) -> bool {
    let bytes = query.as_bytes();
    if bytes.is_empty() {
        return false;
    }

    // Starts uppercase → PascalCase type/class name
    if bytes[0].is_ascii_uppercase() {
        return true;
    }

    // Contains :: or . → qualified symbol
    if query.contains("::") || query.contains('.') {
        return true;
    }

    // Contains underscore → snake_case identifier
    if query.contains('_') {
        return true;
    }

    // Contains hyphen → kebab-case identifier (component names, npm packages)
    if query.contains('-') {
        return true;
    }

    // Starts with $ or @ → special symbol
    if bytes[0] == b'$' || bytes[0] == b'@' {
        return true;
    }

    // camelCase: starts lowercase but has uppercase later → likely function/method name
    if bytes[0].is_ascii_lowercase() && bytes[1..].iter().any(u8::is_ascii_uppercase) {
        return true;
    }

    // Short all-lowercase without any symbol markers → concept, not symbol
    // e.g. "thinking", "alias", "cli", "mode", "config"
    false
}

/// Does this query look like a filename? Has an extension, or matches known extensionless names.
fn looks_like_filename(query: &str) -> bool {
    if query.contains(' ') || query.contains('/') {
        return false;
    }
    // Has a dot followed by an extension (not just a dotfile)
    if let Some(dot_pos) = query.rfind('.') {
        if dot_pos > 0 && dot_pos < query.len() - 1 {
            return true;
        }
    }
    // Known extensionless filenames
    matches!(
        query,
        "README"
            | "LICENSE"
            | "Makefile"
            | "GNUmakefile"
            | "Dockerfile"
            | "Containerfile"
            | "Vagrantfile"
            | "Rakefile"
            | "Gemfile"
            | "Procfile"
            | "Justfile"
            | "Taskfile"
            | "CHANGELOG"
            | "CONTRIBUTING"
            | "AUTHORS"
            | "CODEOWNERS"
    )
}

/// Does the pattern contain regex metacharacters?
/// Used to distinguish `/pattern/` regex from `/path/` paths.
fn has_regex_metachar(s: &str) -> bool {
    s.bytes().any(|b| {
        matches!(
            b,
            b'(' | b')'
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b'*'
                | b'+'
                | b'?'
                | b'|'
                | b'\\'
                | b'^'
                | b'$'
        )
    })
}

/// Known source-file extensions. If `after` matches one of these, treat as filename.
/// Anything else with identifier on both sides → dotted symbol (method/property access).
const FILE_EXTENSIONS: &[&str] = &[
    "rs", "go", "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs", "java", "kt", "kts", "scala",
    "swift", "rb", "php", "cs", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "m", "mm", "lua",
    "dart", "ex", "exs", "erl", "hrl", "elm", "hs", "clj", "cljs", "cljc", "ml", "mli", "fs",
    "fsi", "fsx", "vb", "pas", "pl", "pm", "r", "jl", "nim", "zig", "v", "sh", "bash", "zsh",
    "fish", "ps1", "bat", "cmd", "html", "htm", "css", "scss", "sass", "less", "vue", "svelte",
    "astro", "md", "mdx", "rst", "adoc", "txt", "tex", "yaml", "yml", "toml", "json", "jsonc",
    "json5", "xml", "ini", "cfg", "conf", "env", "proto", "graphql", "gql", "sql", "prisma",
    "wasm", "lock",
];

/// Is this a dotted symbol (method/property access) rather than a filename?
/// "Auth.validate", "std.path" → true (symbol)
/// "server.go", "config.yaml" → false (filename)
fn is_dotted_symbol(query: &str) -> bool {
    let Some(dot_pos) = query.rfind('.') else {
        return false;
    };
    if dot_pos == 0 || dot_pos >= query.len() - 1 {
        return false;
    }
    let before = &query[..dot_pos];
    let after = &query[dot_pos + 1..];
    // Both parts must be identifiers
    if !is_identifier(before) || !is_identifier(after) {
        return false;
    }
    // If `after` is a known file extension → filename, not symbol.
    let after_lower = after.to_ascii_lowercase();
    !FILE_EXTENSIONS.contains(&after_lower.as_str())
}

/// Identifier check without regex: first byte is [a-zA-Z_$@],
/// rest are [a-zA-Z0-9_$\.\-]. Tight loop over bytes.
pub fn is_identifier(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let first_valid = matches!(
        bytes[0],
        b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' | b'@'
    );
    first_valid
        && bytes[1..].iter().all(|&b| {
            matches!(
                b,
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'$' | b'.' | b'-'
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn regex_patterns() {
        let scope = PathBuf::from(".");
        assert!(matches!(
            classify("/render(Call|Result)/", &scope),
            QueryType::Regex(_)
        ));
        assert!(matches!(
            classify("/renderC[a-z]+/", &scope),
            QueryType::Regex(_)
        ));
        assert!(matches!(
            classify("/renderC[a-z]{3}/", &scope),
            QueryType::Regex(_)
        ));
        assert!(matches!(
            classify("/renderC.*/", &scope),
            QueryType::Regex(_)
        ));
        // Single slash or empty pattern should not be regex
        assert!(!matches!(classify("//", &scope), QueryType::Regex(_)));
        // Inner slashes = path, not regex
        assert!(!matches!(
            classify("/src/lib.rs/", &scope),
            QueryType::Regex(_)
        ));
        assert!(!matches!(classify("/src/", &scope), QueryType::Regex(_)));
    }

    #[test]
    fn glob_patterns() {
        let scope = PathBuf::from(".");
        assert!(matches!(classify("*.test.ts", &scope), QueryType::Glob(_)));
        assert!(matches!(
            classify("src/**/*.rs", &scope),
            QueryType::Glob(_)
        ));
        assert!(matches!(classify("{a,b}.js", &scope), QueryType::Glob(_)));
    }

    #[test]
    fn identifiers() {
        let scope = PathBuf::from(".");
        assert!(matches!(
            classify("handleAuth", &scope),
            QueryType::Symbol(_)
        ));
        assert!(matches!(
            classify("handle_auth", &scope),
            QueryType::Symbol(_)
        ));
        assert!(matches!(
            classify("my-component", &scope),
            QueryType::Symbol(_)
        ));
        assert!(matches!(
            classify("AuthService.validate", &scope),
            QueryType::Symbol(_)
        ));
        assert!(matches!(classify("$ref", &scope), QueryType::Symbol(_)));
        assert!(matches!(classify("@types", &scope), QueryType::Symbol(_)));
    }

    #[test]
    fn content_queries() {
        let scope = PathBuf::from(".");
        assert!(matches!(classify("404", &scope), QueryType::Content(_)));
        assert!(matches!(
            classify("TODO: fix this", &scope),
            QueryType::Content(_)
        ));
        assert!(matches!(
            classify("import { X }", &scope),
            QueryType::Content(_)
        ));
    }

    #[test]
    fn concept_queries() {
        let scope = PathBuf::from(".");
        // Single lowercase words → concept, not symbol
        assert!(matches!(
            classify("thinking", &scope),
            QueryType::Concept(_)
        ));
        assert!(matches!(classify("alias", &scope), QueryType::Concept(_)));
        assert!(matches!(classify("cli", &scope), QueryType::Concept(_)));
        assert!(matches!(classify("mode", &scope), QueryType::Concept(_)));
        assert!(matches!(classify("config", &scope), QueryType::Concept(_)));
        assert!(matches!(classify("server", &scope), QueryType::Concept(_)));
        // Multi-word phrases → concept
        assert!(matches!(
            classify("cli mode", &scope),
            QueryType::Concept(_)
        ));
        assert!(matches!(
            classify("search flow", &scope),
            QueryType::Concept(_)
        ));
        assert!(matches!(
            classify("model mapping", &scope),
            QueryType::Concept(_)
        ));
    }

    #[test]
    fn symbol_not_concept() {
        let scope = PathBuf::from(".");
        // PascalCase → symbol
        assert!(matches!(
            classify("SearchResult", &scope),
            QueryType::Symbol(_)
        ));
        assert!(matches!(classify("MapModel", &scope), QueryType::Symbol(_)));
        // camelCase → symbol
        assert!(matches!(
            classify("handleAuth", &scope),
            QueryType::Symbol(_)
        ));
        assert!(matches!(
            classify("thinkingBudget", &scope),
            QueryType::Symbol(_)
        ));
        // snake_case → symbol
        assert!(matches!(
            classify("is_test_file", &scope),
            QueryType::Symbol(_)
        ));
        assert!(matches!(
            classify("handle_auth", &scope),
            QueryType::Symbol(_)
        ));
        // dotted → symbol
        assert!(matches!(
            classify("Auth.validate", &scope),
            QueryType::Symbol(_)
        ));
    }

    #[test]
    fn is_identifier_checks() {
        assert!(is_identifier("handleAuth"));
        assert!(is_identifier("_private"));
        assert!(is_identifier("$ref"));
        assert!(is_identifier("@decorator"));
        assert!(is_identifier("my-component"));
        assert!(is_identifier("Auth.validate"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("has space"));
        assert!(!is_identifier("123start"));
    }

    #[test]
    fn or_pattern_queries() {
        let scope = PathBuf::from(".");
        // Pipe-separated identifiers → regex
        assert!(matches!(
            classify("Config|Security|Auth", &scope),
            QueryType::Regex(_)
        ));
        assert!(matches!(
            classify("handleAuth|handleLogin", &scope),
            QueryType::Regex(_)
        ));
        assert!(matches!(
            classify("TODO|FIXME|HACK", &scope),
            QueryType::Regex(_)
        ));
        // Single part with trailing pipe → not regex (only 1 non-empty part)
        assert!(!matches!(classify("Foo|", &scope), QueryType::Regex(_)));
        // Non-identifier parts → not regex
        assert!(!matches!(
            classify("has space|also space", &scope),
            QueryType::Regex(_)
        ));
        // Already /wrapped/ → regex via step 0, not this check
        assert!(matches!(classify("/Foo|Bar/", &scope), QueryType::Regex(_)));
    }

    #[test]
    fn bare_filename_glob_fallback() {
        // File that doesn't exist at scope root → glob fallback
        let scope = PathBuf::from(".");
        match classify("ProgramDB.java", &scope) {
            QueryType::FilePath(_) => {
                // Also acceptable if file happens to exist
            }
            QueryType::Glob(pattern) => {
                assert_eq!(pattern, "**/ProgramDB.java");
            }
            other => panic!("expected FilePath or Glob, got {other:?}"),
        }
        // Known extensionless filename that doesn't exist → glob
        match classify("Dockerfile", &scope) {
            QueryType::FilePath(_) => {} // exists in tilth repo
            QueryType::Glob(pattern) => {
                assert_eq!(pattern, "**/Dockerfile");
            }
            other => panic!("expected FilePath or Glob, got {other:?}"),
        }
    }
}
