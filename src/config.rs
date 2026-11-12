//! tilth's configuration. `main` builds one [`Config`] at startup from the
//! environment and the command-line flags, and passes it, or the field a
//! function needs, to every use. Nothing else reads the environment.

use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use tilth_core::search::walk::WalkConfig;

/// Settings that hold for the whole run, CLI or MCP server.
///
/// An environment variable that does not parse falls back to its default,
/// without a warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// How walks run. `TILTH_THREADS` sets the worker threads, for walks and
    /// for the rayon pool; `0` lets each choose. `TILTH_RESPECT_GITIGNORE`
    /// set to `1`, `true`, `yes` or `on` (in any case), or the
    /// `--respect-gitignore` flag, makes walks honor gitignore-style files.
    pub walk: WalkConfig,
    /// Files above this many bytes get an outline instead of their full text
    /// on a full read, so a multi-megabyte response cannot time out an MCP
    /// client. Grok shares the cap to bound its decode and parse.
    /// `TILTH_FULL_SIZE_CAP`, in bytes.
    pub full_size_cap: u64,
    /// How long one MCP tool call may run before the server answers with a
    /// timeout. `TILTH_TIMEOUT`, in seconds.
    pub request_timeout: Duration,
    /// How long one batch read in MCP may run. It is checked between files.
    /// `TILTH_BATCH_TIMEOUT`, in seconds.
    pub batch_timeout: Duration,
    /// Whether the MCP initialize response starts with the project
    /// fingerprint. `TILTH_NO_OVERVIEW`, set to any value, or the
    /// `--no-overview` flag turns it off.
    pub overview: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            walk: WalkConfig::default(),
            full_size_cap: 2_000_000,
            request_timeout: Duration::from_secs(90),
            batch_timeout: Duration::from_secs(60),
            overview: true,
        }
    }
}

impl Config {
    /// The configuration the environment describes. `env` returns a
    /// variable's value, or `None` when it is unset or not Unicode.
    #[must_use]
    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
        let default = Self::default();
        let secs = |name, fallback| parse(env(name)).map_or(fallback, Duration::from_secs);
        Self {
            walk: WalkConfig {
                threads: parse(env("TILTH_THREADS"))
                    .map_or(default.walk.threads, NonZeroUsize::new),
                respect_gitignore: env("TILTH_RESPECT_GITIGNORE").is_some_and(|v| {
                    matches!(
                        v.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                }),
            },
            full_size_cap: parse(env("TILTH_FULL_SIZE_CAP")).unwrap_or(default.full_size_cap),
            request_timeout: secs("TILTH_TIMEOUT", default.request_timeout),
            batch_timeout: secs("TILTH_BATCH_TIMEOUT", default.batch_timeout),
            overview: env("TILTH_NO_OVERVIEW").is_none(),
        }
    }
}

fn parse<T: FromStr>(value: Option<String>) -> Option<T> {
    value?.parse().ok()
}

/// A variable from the process environment, or `None` when it is unset or
/// not Unicode. This is the one place tilth reads its environment:
/// [`Config::from_env`] takes it as its lookup, and the CLI's pager and
/// terminal height and the installer's Windows `APPDATA` read through it
/// too. Those three locate things on the host, as `HOME` does, rather than
/// configure tilth, so they stay at their one use.
#[must_use]
pub fn env_var(name: &str) -> Option<String> {
    #[expect(
        clippy::disallowed_methods,
        reason = "by design: the one read of the process environment"
    )]
    std::env::var(name).ok()
}

/// The directory the process was started in. This is the one read of the
/// working directory: `main` calls it once, and the project root it yields
/// is passed to everything that resolves a path. Nothing changes the working
/// directory, so the relative paths the CLI hands the OS resolve against
/// this same directory.
///
/// # Errors
///
/// When the directory cannot be read, for example because it was deleted.
/// The error says so, and every use reports it as it is.
pub fn launch_dir() -> io::Result<PathBuf> {
    #[expect(
        clippy::disallowed_methods,
        reason = "by design: the one read of the process working directory"
    )]
    std::env::current_dir()
        .map_err(|e| io::Error::new(e.kind(), format!("cannot read the current directory: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(vars: &[(&str, &str)]) -> Config {
        Config::from_env(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    #[test]
    fn an_empty_environment_gives_the_defaults() {
        assert_eq!(config(&[]), Config::default());
    }

    #[test]
    fn each_variable_sets_its_field() {
        let c = config(&[
            ("TILTH_THREADS", "3"),
            ("TILTH_RESPECT_GITIGNORE", "1"),
            ("TILTH_FULL_SIZE_CAP", "100"),
            ("TILTH_TIMEOUT", "7"),
            ("TILTH_BATCH_TIMEOUT", "8"),
            ("TILTH_NO_OVERVIEW", "1"),
        ]);
        assert_eq!(
            c,
            Config {
                walk: WalkConfig {
                    threads: NonZeroUsize::new(3),
                    respect_gitignore: true,
                },
                full_size_cap: 100,
                request_timeout: Duration::from_secs(7),
                batch_timeout: Duration::from_secs(8),
                overview: false,
            }
        );
    }

    #[test]
    fn a_value_that_does_not_parse_falls_back_to_the_default() {
        let c = config(&[
            ("TILTH_THREADS", "many"),
            ("TILTH_FULL_SIZE_CAP", "-1"),
            ("TILTH_TIMEOUT", "1.5"),
            ("TILTH_BATCH_TIMEOUT", ""),
        ]);
        assert_eq!(c, Config::default());
    }

    /// The walker and the rayon pool both read 0 threads as "choose for me".
    #[test]
    fn zero_threads_lets_the_libraries_choose() {
        assert_eq!(config(&[("TILTH_THREADS", "0")]).walk.threads, None);
    }

    #[test]
    fn gitignore_takes_the_usual_true_words_in_any_case() {
        for on in ["1", "true", " YES ", "On"] {
            let c = config(&[("TILTH_RESPECT_GITIGNORE", on)]);
            assert!(c.walk.respect_gitignore, "{on:?}");
        }
        for off in ["0", "false", "", "enabled"] {
            let c = config(&[("TILTH_RESPECT_GITIGNORE", off)]);
            assert!(!c.walk.respect_gitignore, "{off:?}");
        }
    }

    /// Any value, even an empty one, turns the overview off.
    #[test]
    fn no_overview_needs_only_to_be_set() {
        assert!(!config(&[("TILTH_NO_OVERVIEW", "")]).overview);
        assert!(!config(&[("TILTH_NO_OVERVIEW", "0")]).overview);
    }
}
