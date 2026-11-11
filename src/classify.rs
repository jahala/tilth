use std::path::Path;

use crate::types::QueryType;

/// Classify a query string into a `QueryType` by byte-pattern matching.
/// No regex engine — `matches!` compiles to a jump table.
pub fn classify(query: &str, scope: &Path) -> QueryType {
    // 0. Slash-wrapped regex — /pattern/ or /pattern/flags → regex content search.
    //    Must come before glob check: regex metacharacters ([, {, *) overlap with glob syntax.
    //    Bare /pattern/ counts only if the pattern contains regex metacharacters — otherwise
    //    /src/ would be misclassified as regex instead of a path. Trailing flags (`/week/i`)
    //    are a regex by intent, so they need no metacharacter; the flags become an inline
    //    `(?flags)` group. An existing path whose last component happens to spell flags
    //    (`/tmp/s`) stays a path.
    if query.len() >= 3 && query.starts_with('/') {
        if let Some(cut) = query[1..].rfind('/') {
            let cut = cut + 1;
            let pattern = &query[1..cut];
            let flags = &query[cut + 1..];
            if !pattern.is_empty() {
                if flags.is_empty() {
                    if has_regex_metachar(pattern) {
                        return QueryType::Regex(pattern.into());
                    }
                } else if flags
                    .bytes()
                    .all(|b| matches!(b, b'i' | b'm' | b's' | b'x' | b'U'))
                    && !scope.join(query).try_exists().unwrap_or(false)
                {
                    return QueryType::Regex(format!("(?{flags}){pattern}"));
                }
            }
        }
    }

    // 1. Glob — check first because globs can contain path separators.
    //    But only if no spaces: real globs don't have spaces, content like "import { X }" does.
    //    Glob characters in a shape no file name carries (`Searcher.*Builder`,
    //    `[0-9]+`) make the query a regex: listing files for it always found none.
    if !query.contains(' ')
        && query
            .bytes()
            .any(|b| matches!(b, b'*' | b'?' | b'{' | b'['))
    {
        if is_regex_shaped(query) {
            return QueryType::Regex(query.into());
        }
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
    }

    // 6. Identifier — no whitespace, starts with letter/underscore/$/@ — or a
    //    `::` path of identifiers (`B::run`, `std::path::Path`).
    if is_symbol_name(query) {
        // Sub-classify: exact symbol vs concept
        if looks_like_exact_symbol(query) {
            return QueryType::Symbol(query.into());
        }
        return QueryType::Concept(query.into());
    }

    // 7. Multi-word — could be concept phrase ("cli mode", "search flow")
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

    // 8. Everything else
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

/// Does a query that carries glob characters use syntax that only a regex
/// has? Each signal is something a file glob never needs:
///
/// - `|` alternation, `^` at the start or `$` at the end;
/// - `.*` or `.?` with name characters on both sides (`Searcher.*Builder`).
///   `Cargo.*`, `*.test.*`, `.*rc` and `foo.*.bak` are file globs, because
///   there the dot starts an extension or a dotfile;
/// - a `+` after `.`, `]` or `)`;
/// - a `{n}` or `{n,m}` repeat count after `]` or `)`, or after `.` unless
///   it is two numbers closing the query or a path component: `*.{1,8}` is
///   a glob for the extensions 1 and 8, while `a.{2,4}b` and `x.{3}` are
///   regexes;
/// - a backslash escape (`\w`, `\(`, `\.`), except on Windows, where a
///   backslash separates path components. Escaping a glob character
///   (`\[`, `\*`) is a glob escape too, so it counts only when the query
///   has no glob character left unescaped: `app/\[slug\]/*.tsx` is a glob,
///   `\[TODO\]` a regex.
///
/// Parentheses alone do not count: `app/(auth)/*.tsx` is a route-group glob.
fn is_regex_shaped(query: &str) -> bool {
    let b = query.as_bytes();
    let name_char = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let glob_char = |c: u8| matches!(c, b'*' | b'?' | b'[' | b']' | b'{' | b'}');
    if b.contains(&b'|') || b.first() == Some(&b'^') || b.last() == Some(&b'$') {
        return true;
    }
    let mut glob_escape = false;
    let mut unescaped_glob = false;
    let mut i = 0;
    while i < b.len() {
        let prev = i.checked_sub(1).map(|j| b[j]);
        let next = b.get(i + 1).copied();
        match b[i] {
            b'.' if matches!(next, Some(b'*' | b'?'))
                && prev.is_some_and(name_char)
                && b.get(i + 2).copied().is_some_and(name_char) =>
            {
                return true;
            }
            b'+' if matches!(prev, Some(b'.' | b']' | b')')) => return true,
            b'{' if matches!(prev, Some(b']' | b')')) && repeat_count(&b[i + 1..]).is_some() => {
                return true;
            }
            b'{' if prev == Some(b'.') => {
                if let Some(count) = repeat_count(&b[i + 1..]) {
                    let after = b.get(i + 1 + count.len).copied();
                    let alternation = count.two_numbers && matches!(after, None | Some(b'/'));
                    if !alternation {
                        return true;
                    }
                }
            }
            b'\\' if !cfg!(windows) && next.is_some_and(glob_char) => {
                glob_escape = true;
                i += 2;
                continue;
            }
            b'\\' if !cfg!(windows) && next.is_some_and(|c| c.is_ascii_graphic()) => return true,
            _ => {}
        }
        unescaped_glob |= glob_char(b[i]);
        i += 1;
    }
    glob_escape && !unescaped_glob
}

/// A regex repeat count right after a `{`: `n}`, `n,}` or `n,m}`.
struct RepeatCount {
    /// Bytes from after the `{` to after the `}`.
    len: usize,
    /// `n,m}`, which a glob reads as two alternatives.
    two_numbers: bool,
}

/// `rest` starts right after a `{`: is it a regex repeat count?
fn repeat_count(rest: &[u8]) -> Option<RepeatCount> {
    let close = rest.iter().position(|&c| c == b'}')?;
    let mut parts = rest[..close].split(|&c| c == b',');
    let min = parts.next().unwrap_or_default();
    let max = parts.next();
    let digits = |p: &[u8]| p.iter().all(u8::is_ascii_digit);
    let valid = !min.is_empty() && digits(min) && max.is_none_or(digits) && parts.next().is_none();
    valid.then(|| RepeatCount {
        len: close + 1,
        two_numbers: max.is_some_and(|m| !m.is_empty()),
    })
}

/// An identifier, or a `::` path of identifiers (`B::run`). Every segment
/// must be a non-empty identifier, so `TODO:` and `a::::b` are not names.
pub fn is_symbol_name(s: &str) -> bool {
    if s.contains("::") {
        s.split("::").all(is_identifier)
    } else {
        is_identifier(s)
    }
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

    /// `/pattern/flags` is a regex with inline flags. `/week/i` used to fall
    /// through to a path lookup and dead-end as "not found".
    #[test]
    fn regex_with_trailing_flags() {
        let scope = PathBuf::from(".");
        assert!(matches!(
            classify("/week/i", &scope),
            QueryType::Regex(p) if p == "(?i)week"
        ));
        assert!(matches!(
            classify("/a.b/im", &scope),
            QueryType::Regex(p) if p == "(?im)a.b"
        ));
        // Letters that are not regex flags are not flags.
        assert!(!matches!(classify("/week/z", &scope), QueryType::Regex(_)));
        assert!(!matches!(classify("/usr/bin", &scope), QueryType::Regex(_)));
        // An existing path whose last component looks like a flag stays a path.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("i")).unwrap();
        let existing = format!("{}/i", tmp.path().display());
        assert!(
            matches!(classify(&existing, &scope), QueryType::FilePath(_)),
            "an existing path must not become a regex"
        );
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

    /// A query with glob characters that no file name would carry is a
    /// regex. `Searcher.*Builder` used to list "0 files" while
    /// `/Searcher.*Builder/` found the matches.
    #[test]
    fn regex_shaped_queries_are_not_globs() {
        let scope = PathBuf::from(".");
        for query in [
            "Searcher.*Builder",
            "impl.*for",
            "foo.?bar",
            "[0-9]+",
            #[cfg(not(windows))]
            "handle[A-Z]\\w+",
            #[cfg(not(windows))]
            "Err\\(.*\\)",
            "get(User|Account)*",
            "^test_*",
            #[cfg(not(windows))]
            "log\\.[a-z]*",
            "fn.+main*",
            "[0-9]{3}*",
            "a.{2,4}b",
            "x.{3}",
            "x.{2,}",
            #[cfg(not(windows))]
            "\\[TODO\\]",
        ] {
            assert!(
                matches!(classify(query, &scope), QueryType::Regex(ref p) if p == query),
                "{query:?} should be a regex, got {:?}",
                classify(query, &scope)
            );
        }
    }

    /// File globs stay globs, including the shapes that share characters
    /// with regex: an extension wildcard after a dot, a dotfile prefix and
    /// route-group parentheses.
    #[test]
    fn file_globs_stay_globs() {
        let scope = PathBuf::from(".");
        for query in [
            "*.rs",
            "**/*.rs",
            "src/**/*.ts",
            "*.{rs,ts}",
            "*_test.go",
            "test_*.py",
            "Cargo.*",
            "*.test.*",
            "*.*",
            ".*rc",
            "src/.*rc",
            "foo.*.bak",
            "[Mm]akefile",
            "*.[ch]",
            "file?.txt",
            "app/(auth)/*.tsx",
            "include/c++/*",
            "TODO.*",
            "man/*.{1,8}",
            "*.{1,2}",
            #[cfg(not(windows))]
            "app/\\[slug\\]/*.tsx",
        ] {
            assert!(
                matches!(classify(query, &scope), QueryType::Glob(_)),
                "{query:?} should stay a glob, got {:?}",
                classify(query, &scope)
            );
        }
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
    fn double_colon_paths_are_symbols() {
        // `Type::method` is the qualified name grok documents; search takes it
        // as a symbol too, with `Type.method`.
        let scope = PathBuf::from(".");
        for query in ["B::run", "std::path::Path", "a::b.c", "Vec::new"] {
            assert!(
                matches!(classify(query, &scope), QueryType::Symbol(ref q) if q == query),
                "{query}"
            );
        }
        // A lone colon or an empty segment is not a path.
        for query in ["TODO:", "a:b", "::run", "B::", "a::::b"] {
            assert!(
                !matches!(classify(query, &scope), QueryType::Symbol(_)),
                "{query}"
            );
        }
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
}
