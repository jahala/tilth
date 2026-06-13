pub mod detection;
pub mod outline;
pub mod treesitter;

pub(crate) mod spec;

pub(crate) mod c;
pub(crate) mod cpp;
pub(crate) mod csharp;
pub(crate) mod dockerfile;
pub(crate) mod elixir;
pub(crate) mod go;
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

use std::path::Path;

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
];

/// Test-only accessor for the full `Lang` set, so `spec`'s table-invariant
/// tests can iterate every variant without making `ALL_LANGS` public.
#[cfg(test)]
pub(crate) fn mod_all_langs_for_test() -> &'static [Lang] {
    ALL_LANGS
}

/// Detect file type by extension, then by name.
pub fn detect_file_type(path: &Path) -> FileType {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            // Code extensions come from each language's `spec.extensions`.
            for &lang in ALL_LANGS {
                if spec::spec(lang).extensions.contains(&ext) {
                    return FileType::Code(lang);
                }
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
pub(crate) fn package_root(path: &Path) -> Option<&Path> {
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
