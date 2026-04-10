use memchr::memmem;

use crate::run::types::{
    build_lint_summary, truncate_detail, Counts, DetectResult, Diagnostic, ParsedOutput, Severity,
};

use super::Parser;

pub static PARSER: PipParser = PipParser;

pub struct PipParser;

impl Parser for PipParser {
    fn name(&self) -> &'static str {
        "pip-install"
    }

    /// Detect pip install output via byte scanning — no regex.
    fn detect(&self, sample: &str) -> DetectResult {
        let bytes = sample.as_bytes();

        // Successful install summary
        let success = memmem::Finder::new("Successfully installed");
        if success.find(bytes).is_some() {
            return DetectResult::Text;
        }

        // Already satisfied (common in incremental installs)
        let satisfied = memmem::Finder::new("Requirement already satisfied");
        if satisfied.find(bytes).is_some() {
            return DetectResult::Text;
        }

        // Download progress — pip-specific prefix before a package name
        let collecting = memmem::Finder::new("Collecting ");
        let downloading = memmem::Finder::new("Downloading ");
        let cached = memmem::Finder::new("Using cached ");
        if collecting.find(bytes).is_some()
            && (downloading.find(bytes).is_some() || cached.find(bytes).is_some())
        {
            return DetectResult::Text;
        }

        // pip error prefix
        let error = memmem::Finder::new("ERROR: ");
        if error.find(bytes).is_some() {
            return DetectResult::Text;
        }

        DetectResult::NoMatch
    }

    fn parse(&self, input: &str, _hint: DetectResult) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let mut installed_packages: Vec<&str> = Vec::new();
        let mut collecting_count: u32 = 0;
        let mut already_satisfied_count: u32 = 0;

        // Finders for line classification.
        let success_finder = memmem::Finder::new("Successfully installed");
        let satisfied_finder = memmem::Finder::new("Requirement already satisfied");
        let collecting_finder = memmem::Finder::new("Collecting ");
        let error_finder = memmem::Finder::new("ERROR: ");

        let mut err_block: Vec<String> = Vec::new();

        let flush_err_block = |block: &mut Vec<String>, diags: &mut Vec<Diagnostic>| {
            if block.is_empty() {
                return;
            }
            let joined = block.join("\n");
            let message = block.first().map_or_else(
                || "pip error".to_string(),
                |s| s.trim_start_matches("ERROR:").trim().to_string(),
            );
            let detail = if block.len() > 1 {
                truncate_detail(&joined)
            } else {
                None
            };
            diags.push(Diagnostic {
                severity: Severity::Error,
                location: None,
                name: "ERROR".to_string(),
                message,
                detail,
            });
            block.clear();
        };

        for line in input.lines() {
            let bytes = line.as_bytes();

            // Flush error block when we leave ERROR lines.
            let is_error_line = error_finder.find(bytes).is_some();
            if !is_error_line && !err_block.is_empty() {
                flush_err_block(&mut err_block, &mut diagnostics);
            }

            if is_error_line {
                err_block.push(line.to_string());
                continue;
            }

            // "Successfully installed pkg1-1.0 pkg2-2.0 ..."
            if success_finder.find(bytes).is_some() {
                if let Some(rest) = line.find("Successfully installed").map(|i| &line[i + 22..]) {
                    installed_packages = rest.split_whitespace().collect();
                }
                continue;
            }

            // "Requirement already satisfied: ..." — count only, don't keep lines.
            if satisfied_finder.find(bytes).is_some() {
                already_satisfied_count += 1;
                continue;
            }

            // "Collecting <pkg>" — count only; rest are noise — drop.
            if collecting_finder.find(bytes).is_some() {
                collecting_count += 1;
            }
        }

        // Flush trailing error block.
        flush_err_block(&mut err_block, &mut diagnostics);

        // Build summary.
        let errors = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count() as u32;

        let summary = build_install_summary(
            &installed_packages,
            already_satisfied_count,
            collecting_count,
            errors,
        );

        ParsedOutput {
            tool: "pip-install",
            summary,
            diagnostics,
            counts: Counts {
                errors,
                ..Counts::default()
            },
            duration_secs: None,
            raw_lines,
            raw_bytes,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_install_summary(
    installed: &[&str],
    already_satisfied: u32,
    collected: u32,
    errors: u32,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if !installed.is_empty() {
        parts.push(format!("installed {} package(s)", installed.len()));
    } else if collected > 0 {
        parts.push(format!("collected {collected} package(s)"));
    }

    if already_satisfied > 0 {
        parts.push(format!("{already_satisfied} already satisfied"));
    }

    if errors > 0 {
        parts.push(build_lint_summary(errors, 0));
    }

    if parts.is_empty() {
        "install complete".to_string()
    } else {
        parts.join("; ")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Detection -----------------------------------------------------------

    #[test]
    fn detect_successfully_installed() {
        let sample = "Successfully installed requests-2.28.0 certifi-2022.12.7\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_requirement_satisfied() {
        let sample = "Requirement already satisfied: pip in /usr/lib/python3/dist-packages\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_collecting_and_downloading() {
        let sample = concat!(
            "Collecting requests==2.28.0\n",
            "  Downloading requests-2.28.0-py3-none-any.whl (62 kB)\n",
        );
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_pip_error() {
        let sample =
            "ERROR: Could not find a version that satisfies the requirement nonexistent==0.0.0\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_rejects_generic() {
        let sample = "Building project...\nCompiling foo\nDone!\n";
        assert!(!PARSER.detect(sample).matched());
    }

    // -- Parse ---------------------------------------------------------------

    #[test]
    fn parse_clean_install() {
        let input = concat!(
            "Collecting requests==2.28.0\n",
            "  Downloading requests-2.28.0-py3-none-any.whl (62 kB)\n",
            "Collecting certifi>=2017.4.17\n",
            "  Using cached certifi-2022.12.7-py3-none-any.whl (155 kB)\n",
            "Installing collected packages: certifi, requests\n",
            "Successfully installed certifi-2022.12.7 requests-2.28.0\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.tool, "pip-install");
        assert_eq!(out.counts.errors, 0);
        assert!(out.diagnostics.is_empty());
        assert!(
            out.summary.contains("installed 2 package(s)"),
            "summary: {}",
            out.summary
        );
    }

    #[test]
    fn parse_already_satisfied() {
        let input = concat!(
            "Requirement already satisfied: requests in /usr/lib/python3/dist-packages\n",
            "Requirement already satisfied: certifi in /usr/lib/python3/dist-packages\n",
            "Requirement already satisfied: urllib3 in /usr/lib/python3/dist-packages\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.counts.errors, 0);
        assert!(out.diagnostics.is_empty());
        assert!(
            out.summary.contains("3 already satisfied"),
            "summary: {}",
            out.summary
        );
    }

    #[test]
    fn parse_with_error() {
        let input = concat!(
            "Collecting nonexistent==0.0.0\n",
            "ERROR: No matching distribution found for nonexistent==0.0.0\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.counts.errors, 1);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.severity, Severity::Error);
        assert!(
            diag.message.contains("No matching distribution"),
            "message: {}",
            diag.message
        );
    }

    #[test]
    fn parse_dependency_conflict() {
        let input = concat!(
            "Collecting package-a==1.0\n",
            "ERROR: pip's dependency resolver does not currently take into account all the packages that are installed. This behaviour is the source of the following dependency conflicts.\n",
            "package-b 2.0 requires package-a>=2.0, but you have package-a 1.0 which is incompatible.\n",
            "Successfully installed package-a-1.0\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        // Should have one error for the dependency conflict.
        assert_eq!(out.counts.errors, 1);
        assert_eq!(out.diagnostics.len(), 1);
        assert_eq!(out.diagnostics[0].severity, Severity::Error);
        // And still report the successful install.
        assert!(
            out.summary.contains("installed 1 package(s)"),
            "summary: {}",
            out.summary
        );
    }
}
