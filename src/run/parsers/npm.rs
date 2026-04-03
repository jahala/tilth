use memchr::memmem;

use crate::run::types::{
    build_lint_summary, truncate_detail, Counts, DetectResult, Diagnostic, ParsedOutput, Severity,
};

use super::Parser;

pub static PARSER: NpmParser = NpmParser;

pub struct NpmParser;

impl Parser for NpmParser {
    fn name(&self) -> &'static str {
        "npm-install"
    }

    /// Detect npm/pnpm/yarn install output via byte scanning — no regex.
    fn detect(&self, sample: &str) -> DetectResult {
        let bytes = sample.as_bytes();

        // npm summary: "added N packages"
        let added = memmem::Finder::new("added ");
        let packages = memmem::Finder::new("packages");
        if added.find(bytes).is_some() && packages.find(bytes).is_some() {
            return DetectResult::Text;
        }

        // npm diagnostics
        let npm_warn = memmem::Finder::new("npm warn");
        let npm_error = memmem::Finder::new("npm error");
        let npm_err_bang = memmem::Finder::new("npm ERR!");
        if npm_warn.find(bytes).is_some()
            || npm_error.find(bytes).is_some()
            || npm_err_bang.find(bytes).is_some()
        {
            return DetectResult::Text;
        }

        // pnpm fingerprints
        let pnpm_ready = memmem::Finder::new("packages are ready");
        let pnpm_progress = memmem::Finder::new("Progress:");
        if pnpm_ready.find(bytes).is_some() || pnpm_progress.find(bytes).is_some() {
            return DetectResult::Text;
        }

        // yarn fingerprints
        let yarn_lockfile = memmem::Finder::new("success Saved lockfile");
        let yarn_yn = memmem::Finder::new("YN0000");
        if yarn_lockfile.find(bytes).is_some() || yarn_yn.find(bytes).is_some() {
            return DetectResult::Text;
        }

        DetectResult::NoMatch
    }

    fn parse(&self, input: &str, _hint: DetectResult) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let mut summary_line: Option<String> = None;
        let mut audit_note: Option<String> = None;

        // Finders for line classification.
        let err_finder = memmem::Finder::new("npm ERR!");
        let warn_finder = memmem::Finder::new("npm warn");
        let warn_upper_finder = memmem::Finder::new("npm WARN");
        let added_finder = memmem::Finder::new("added ");
        let vuln_finder = memmem::Finder::new("vulnerabilit");

        let mut err_block: Vec<String> = Vec::new();

        let flush_err_block = |block: &mut Vec<String>, diags: &mut Vec<Diagnostic>| {
            if block.is_empty() {
                return;
            }
            let joined = block.join("\n");
            let message = block.first().map_or_else(
                || "npm error".to_string(),
                |s| s.trim_start_matches("npm ERR!").trim().to_string(),
            );
            let detail = truncate_detail(&joined);
            diags.push(Diagnostic {
                severity: Severity::Error,
                location: None,
                name: "npm ERR!".to_string(),
                message,
                detail,
            });
            block.clear();
        };

        for line in input.lines() {
            let bytes = line.as_bytes();

            // Flush error block when we exit ERR! lines.
            let is_err_line = err_finder.find(bytes).is_some();
            if !is_err_line && !err_block.is_empty() {
                flush_err_block(&mut err_block, &mut diagnostics);
            }

            if is_err_line {
                err_block.push(line.to_string());
                continue;
            }

            // npm warn / npm WARN — keep as warnings.
            if warn_finder.find(bytes).is_some() || warn_upper_finder.find(bytes).is_some() {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    location: None,
                    name: "npm warn".to_string(),
                    message: line.trim().to_string(),
                    detail: None,
                });
                continue;
            }

            // Audit summary line: "found N vulnerabilities"
            if vuln_finder.find(bytes).is_some() {
                audit_note = Some(line.trim().to_string());
                continue;
            }

            // Install summary line: "added N packages in Xs"
            if added_finder.find(bytes).is_some() && line.contains("package") {
                summary_line = Some(line.trim().to_string());
                continue;
            }

            // pnpm/yarn summary lines.
            if line.contains("packages are ready")
                || line.contains("success Saved lockfile")
                || line.contains("Done in ")
            {
                summary_line = Some(line.trim().to_string());
            }

            // Everything else is noise (progress, resolution lines, etc.) — drop.
        }

        // Flush any trailing error block.
        flush_err_block(&mut err_block, &mut diagnostics);

        // Build summary.
        let errors = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count() as u32;
        let warnings = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count() as u32;

        let summary = if let Some(s) = summary_line {
            let base = if errors > 0 || warnings > 0 {
                format!("{s}; {}", build_lint_summary(errors, warnings))
            } else {
                s
            };
            if let Some(audit) = audit_note {
                format!("{base}; {audit}")
            } else {
                base
            }
        } else if errors > 0 || warnings > 0 {
            build_lint_summary(errors, warnings)
        } else {
            "install complete".to_string()
        };

        ParsedOutput {
            tool: "npm-install",
            summary,
            diagnostics,
            counts: Counts {
                warnings,
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Detection -----------------------------------------------------------

    #[test]
    fn detect_npm_summary() {
        let sample = "added 150 packages in 5s\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_npm_err_bang() {
        let sample = "npm ERR! code ERESOLVE\nnpm ERR! could not resolve\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_pnpm() {
        let sample = "Progress: resolved 200, reused 150, downloaded 10\npackages are ready\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_yarn() {
        let sample = "[YN0000]: Done in 3s\nsuccess Saved lockfile.\n";
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
            "added 150 packages, and audited 151 packages in 5s\n",
            "\n",
            "29 packages are looking for funding\n",
            "  run `npm fund` for details\n",
            "\n",
            "found 0 vulnerabilities\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.tool, "npm-install");
        assert_eq!(out.counts.errors, 0);
        assert_eq!(out.counts.warnings, 0);
        assert!(out.diagnostics.is_empty());
        assert!(
            out.summary.contains("added 150 packages"),
            "summary: {}",
            out.summary
        );
    }

    #[test]
    fn parse_with_warnings() {
        let input = concat!(
            "npm warn deprecated inflight@1.0.6: This module is not supported\n",
            "npm warn deprecated rimraf@2.7.1: Rimraf versions prior to v4 are no longer supported\n",
            "added 200 packages in 8s\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.counts.warnings, 2);
        assert_eq!(out.counts.errors, 0);
        assert_eq!(out.diagnostics.len(), 2);
        assert!(out
            .diagnostics
            .iter()
            .all(|d| d.severity == Severity::Warning));
        assert!(
            out.summary.contains("added 200 packages"),
            "summary: {}",
            out.summary
        );
    }

    #[test]
    fn parse_with_errors() {
        let input = concat!(
            "npm ERR! code ERESOLVE\n",
            "npm ERR! ERESOLVE unable to resolve dependency tree\n",
            "npm ERR!\n",
            "npm ERR! While resolving: my-app@1.0.0\n",
            "npm ERR! Found: react@18.0.0\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.counts.errors, 1);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.severity, Severity::Error);
        assert!(diag.detail.is_some());
        let detail = diag.detail.as_ref().unwrap();
        assert!(detail.contains("ERESOLVE"));
    }

    #[test]
    fn parse_audit_vulnerabilities() {
        let input = concat!(
            "added 100 packages in 3s\n",
            "\n",
            "3 vulnerabilities (1 moderate, 2 high)\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert!(
            out.summary.contains("3 vulnerabilit"),
            "summary: {}",
            out.summary
        );
    }
}
