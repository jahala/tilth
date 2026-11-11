pub mod detection;
pub mod outline;
pub mod treesitter;

pub mod spec;

pub(crate) mod bash;
pub mod c;
pub mod cpp;
pub(crate) mod csharp;
pub(crate) mod dockerfile;
pub mod elixir;
pub mod go;
pub(crate) mod java;
pub(crate) mod javascript;
pub(crate) mod kotlin;
pub(crate) mod make;
pub(crate) mod php;
pub(crate) mod python;
pub(crate) mod ruby;
pub(crate) mod rust;
pub(crate) mod scala;
pub(crate) mod swift;
pub(crate) mod tsx;
pub(crate) mod typescript;

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use crate::types::{FileType, Lang};

/// Every `Lang` variant, in declaration order. Used to scan per-language
/// `spec(lang)` data when building extension / filename / manifest lookups.
const ALL_LANGS: &[Lang] = &[
    Lang::Rust,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::JavaScript,
    Lang::Python,
    Lang::Go,
    Lang::Java,
    Lang::Scala,
    Lang::C,
    Lang::Cpp,
    Lang::Ruby,
    Lang::Php,
    Lang::Swift,
    Lang::Kotlin,
    Lang::CSharp,
    Lang::Elixir,
    Lang::Dockerfile,
    Lang::Make,
    Lang::Bash,
];

/// Test-only accessor for the full `Lang` set, so `spec`'s table-invariant
/// tests can iterate every variant without making `ALL_LANGS` public.
#[cfg(test)]
pub(crate) fn mod_all_langs_for_test() -> &'static [Lang] {
    ALL_LANGS
}

/// Extension → language lookup, built once from every `LangSpec.extensions`.
/// Replaces a per-call linear scan over `ALL_LANGS` with an O(1) hash lookup.
static EXT_TO_LANG: LazyLock<HashMap<&'static str, Lang>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for &lang in ALL_LANGS {
        for &ext in spec::spec(lang).extensions {
            map.entry(ext).or_insert(lang);
        }
    }
    map
});

/// Detect file type by extension, then by name.
pub fn detect_file_type(path: &Path) -> FileType {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            if let Some(&lang) = EXT_TO_LANG.get(ext) {
                return FileType::Code(lang);
            }
            // Non-code extensions stay enumerated here.
            match ext {
                "md" | "mdx" | "rst" => FileType::Markdown,
                "json" | "yaml" | "yml" | "toml" | "xml" | "ini" => FileType::StructuredData,
                "csv" | "tsv" => FileType::Tabular,
                "log" => FileType::Log,
                _ => FileType::Other,
            }
        }
        None => file_type_from_name(path),
    }
}

/// Detect a file's type from its path and content: [`detect_file_type`],
/// with a code language settled by [`refine_lang`]. Use this wherever the
/// content is parsed.
#[must_use]
pub fn detect_file_type_with_content(path: &Path, content: &str) -> FileType {
    match detect_file_type(path) {
        FileType::Code(lang) => FileType::Code(refine_lang(lang, path, content)),
        other => other,
    }
}

/// Settle the language the path gave `path` by its content. The path decides
/// every case but `.h`, which C and C++ share: a header that holds C++ syntax
/// is C++, any other is C.
#[must_use]
pub fn refine_lang(lang: Lang, path: &Path, content: &str) -> Lang {
    if lang == Lang::C && path.extension().is_some_and(|e| e == "h") && cpp::has_cpp_syntax(content)
    {
        Lang::Cpp
    } else {
        lang
    }
}

fn file_type_from_name(path: &Path) -> FileType {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return FileType::Other;
    };
    // Code filenames come from each language's `spec.filenames`.
    for &lang in ALL_LANGS {
        if spec::spec(lang).filenames.contains(&name) {
            return FileType::Code(lang);
        }
    }
    if name.starts_with(".env") {
        return FileType::StructuredData;
    }
    FileType::Other
}

/// Find the nearest package root by looking for manifest files.
///
/// The manifest list is aggregated from every language's `spec.manifests`. A
/// directory is a package root if it contains *any* manifest; order is
/// irrelevant because the matched directory is returned regardless of which
/// manifest hit.
#[must_use]
pub fn package_root(path: &Path) -> Option<&Path> {
    let mut dir = path;
    loop {
        for &lang in ALL_LANGS {
            for m in spec::spec(lang).manifests {
                if dir.join(m).exists() {
                    return Some(dir);
                }
            }
        }
        dir = dir.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(content: &str) -> FileType {
        detect_file_type_with_content(Path::new("x.h"), content)
    }

    #[test]
    fn a_header_holding_cpp_syntax_is_cpp() {
        for src in [
            "class Widget {\n};\n",
            "class EXPORT Widget : public Base {\n};\n",
            "namespace geo {\nint area();\n}\n",
            "namespace {\nint hidden;\n}\n",
            "template <typename T> T id(T x);\n",
            "struct S {\n public:\n  int x;\n};\n",
            "int geo::area() { return 1; }\n",
        ] {
            assert_eq!(header(src), FileType::Code(Lang::Cpp), "{src:?}");
        }
    }

    #[test]
    fn a_header_without_cpp_syntax_stays_c() {
        for src in [
            crate::lang::c::C_DEFINITIONS_FIXTURE,
            // C++ words in comments, strings and as plain C identifiers.
            "/* class Widget { namespace x { template <T> */\nint f(void);\n",
            "// std::vector<int> and class Foo {\nint g(void);\n",
            "const char *s = \"namespace x { a::b }\";\n",
            "struct list *new, *class;\nint private;\n",
            "extern int count;\n",
            // A portable C header guards `extern "C"` for C++ includers; the
            // rest of it is C, and C identifiers such as `new` must stay C.
            "#ifdef __cplusplus\nextern \"C\" {\n#endif\nint f(void);\nstruct node *new;\n",
            "extern \"C\" {\nint f(void);\n}\n",
            // Each attribute in a C23 attribute list.
            "[[gnu::unused, gnu::cold]] int g(void);\n",
            "[[deprecated, gnu::cold]] int h(void);\n",
        ] {
            assert_eq!(header(src), FileType::Code(Lang::C), "{src:?}");
        }
    }

    #[test]
    fn only_headers_are_judged_by_content() {
        let cpp = "class Widget {\n};\n";
        assert_eq!(
            detect_file_type_with_content(Path::new("x.c"), cpp),
            FileType::Code(Lang::C)
        );
        assert_eq!(
            detect_file_type_with_content(Path::new("x.hpp"), "int f(void);\n"),
            FileType::Code(Lang::Cpp)
        );
        assert_eq!(
            detect_file_type_with_content(Path::new("x.rs"), cpp),
            FileType::Code(Lang::Rust)
        );
    }
}
