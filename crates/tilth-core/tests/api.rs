//! The negotiated public surface of `tilth-core`, exercised from outside the
//! crate. Each item here was requested by weed (the garden's diff judge) on
//! 2026-09-05; a consumer with only this crate must be able to do all of it.
//! Four inline language samples plus a two-file callers case.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use tilth_core::{
    analyze_deps, detect_file_type, extract_import_source, find_callers_batch, get_outline_entries,
    is_external, is_import_line, is_test_file, resolve_related_files_with_content, test_entries,
    BloomFilterCache, CallerMatch, Dependent, DepsResult, FileType, Lang, LocalDep, OutlineEntry,
    OutlineKind, TestEntry, TestKind, TilthError,
};

struct Sample {
    file: &'static str,
    lang: Lang,
    source: &'static str,
    function: &'static str,
    import_line: &'static str,
    import_source: &'static str,
}

const SAMPLES: &[Sample] = &[
    Sample {
        file: "greet.ts",
        lang: Lang::TypeScript,
        source: "import { readFileSync } from \"fs\";\n\nexport function greet(name: string): string {\n  return `hello ${name}`;\n}\n\ndescribe(\"greet\", () => {\n  it(\"greets by name\", () => {\n    expect(greet(\"tilth\")).toBe(\"hello tilth\");\n  });\n});\n",
        function: "greet",
        import_line: "import { readFileSync } from \"fs\";",
        import_source: "fs",
    },
    Sample {
        file: "greet.py",
        lang: Lang::Python,
        source: "from os import path\n\n\ndef greet(name):\n    return f\"hello {name}\"\n\n\ndef test_greet():\n    assert greet(\"tilth\") == \"hello tilth\"\n",
        function: "greet",
        import_line: "from os import path",
        import_source: "os",
    },
    Sample {
        file: "greet.rs",
        lang: Lang::Rust,
        source: "use std::fs;\n\npub fn greet(name: &str) -> String {\n    format!(\"hello {name}\")\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn greets() {\n        assert_eq!(super::greet(\"tilth\"), \"hello tilth\");\n    }\n}\n",
        function: "greet",
        import_line: "use std::fs;",
        import_source: "std::fs",
    },
    Sample {
        file: "greet.go",
        lang: Lang::Go,
        source: "package greet\n\nimport \"fmt\"\n\nfunc Greet(name string) string {\n\treturn fmt.Sprintf(\"hello %s\", name)\n}\n\nfunc TestGreet(t *testing.T) {\n\tif Greet(\"tilth\") != \"hello tilth\" {\n\t\tt.Fatal(\"wrong greeting\")\n\t}\n}\n",
        function: "Greet",
        import_line: "import \"fmt\"",
        import_source: "fmt",
    },
];

fn line_of(source: &str, needle: &str) -> u32 {
    let idx = source
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not in sample"));
    u32::try_from(idx + 1).unwrap()
}

fn find_entry<'a>(entries: &'a [OutlineEntry], name: &str) -> Option<&'a OutlineEntry> {
    entries.iter().find_map(|e| {
        if e.name == name {
            Some(e)
        } else {
            find_entry(&e.children, name)
        }
    })
}

#[test]
fn detects_the_four_languages_by_path() {
    for s in SAMPLES {
        assert_eq!(
            detect_file_type(Path::new(s.file)),
            FileType::Code(s.lang),
            "{}",
            s.file
        );
    }
    assert_eq!(detect_file_type(Path::new("notes.md")), FileType::Markdown);
}

#[test]
fn outlines_name_the_function_with_a_plausible_range() {
    for s in SAMPLES {
        let entries = get_outline_entries(s.source, s.lang);
        let entry = find_entry(&entries, s.function)
            .unwrap_or_else(|| panic!("{}: no outline entry named {}", s.file, s.function));
        assert_eq!(entry.kind, OutlineKind::Function, "{}", s.file);
        let def_line = line_of(s.source, s.function);
        assert!(
            entry.start_line <= def_line && def_line <= entry.end_line,
            "{}: {} spans {}-{}, definition is on line {def_line}",
            s.file,
            s.function,
            entry.start_line,
            entry.end_line
        );
    }
}

#[test]
fn import_lines_are_recognised_and_their_source_extracted() {
    for s in SAMPLES {
        assert!(
            is_import_line(s.import_line, s.lang),
            "{}: {:?}",
            s.file,
            s.import_line
        );
        assert!(
            !is_import_line(s.function, s.lang),
            "{}: a bare name is not an import",
            s.file
        );
        assert_eq!(
            extract_import_source(s.import_line, Some(s.lang)),
            s.import_source,
            "{}",
            s.file
        );
        assert!(
            is_external(s.import_source, s.lang),
            "{}: {} is external",
            s.file,
            s.import_source
        );
    }
    assert!(!is_external("./util", Lang::TypeScript));
    assert!(!is_external("crate::util", Lang::Rust));
}

#[test]
fn related_files_resolve_a_relative_typescript_import() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("util.ts"), "export const x = 1;\n").unwrap();
    let main = dir.path().join("main.ts");
    let related = resolve_related_files_with_content(&main, "import { x } from \"./util\";\n");
    assert_eq!(related, vec![dir.path().join("util.ts")]);
}

#[test]
fn test_file_classification_is_by_path() {
    assert!(is_test_file(Path::new("src/greet.test.ts")));
    assert!(is_test_file(Path::new("src/__tests__/greet.ts")));
    assert!(!is_test_file(Path::new("src/greet.ts")));
}

#[test]
fn typescript_test_structure_comes_back_as_data() {
    let ts = &SAMPLES[0];
    let entries: Vec<TestEntry> = test_entries(ts.source, ts.lang);
    assert_eq!(entries.len(), 2, "one suite, one case: {entries:?}");
    let suite = &entries[0];
    assert_eq!(suite.kind, TestKind::Suite);
    assert_eq!(suite.name, "greet");
    assert_eq!(suite.depth, 0);
    assert_eq!(suite.start_line, line_of(ts.source, "describe("));
    assert_eq!(suite.end_line, line_of(ts.source, "});") + 1);
    let case = &entries[1];
    assert_eq!(case.kind, TestKind::Case);
    assert_eq!(case.name, "greets by name");
    assert_eq!(case.depth, 1);
    assert_eq!(case.start_line, line_of(ts.source, "it("));
    assert!(case.end_line >= case.start_line);
    assert!(
        test_entries(SAMPLES[2].source, Lang::Rust).is_empty(),
        "no describe/it walk for rust"
    );
}

#[test]
fn callers_across_two_files_name_the_calling_function_and_line() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.rs"), "pub fn greet() {}\n").unwrap();
    fs::write(
        dir.path().join("b.rs"),
        "pub fn main() {\n    greet();\n}\n",
    )
    .unwrap();
    let bloom = BloomFilterCache::new();
    let targets: HashSet<String> = ["greet".to_string()].into_iter().collect();
    let found: Vec<(String, CallerMatch)> =
        find_callers_batch(&targets, dir.path(), &bloom, None, 50).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let (target, m) = &found[0];
    assert_eq!(target, "greet");
    assert_eq!(m.path.file_name().unwrap(), "b.rs");
    assert_eq!(m.line, 2);
    assert_eq!(m.calling_function, "main");
    assert_eq!(m.call_text, "greet();");
}

#[test]
fn dependency_analysis_runs_on_a_file_in_scope() {
    // The scope is canonical, as the CLI and the MCP server pass it.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "pub fn greet() {}\n").unwrap();
    fs::write(
        root.join("src/b.rs"),
        "use crate::a::greet;\n\npub fn main() {\n    greet();\n}\n",
    )
    .unwrap();
    let bloom = BloomFilterCache::new();
    let result: DepsResult = analyze_deps(&root.join("src/a.rs"), &root, &bloom).unwrap();
    assert_eq!(result.exported_count, 1, "a.rs exports greet");
    let used_by: &Vec<Dependent> = &result.used_by;
    assert_eq!(used_by.len(), 1, "{used_by:?}");
    assert_eq!(used_by[0].path.file_name().unwrap(), "b.rs");
    let _uses: &Vec<LocalDep> = &result.uses_local;
}

#[test]
fn the_error_type_is_a_real_error() {
    fn assert_error<E: std::error::Error + Send + Sync + 'static>() {}
    assert_error::<TilthError>();
}
