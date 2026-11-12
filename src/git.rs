//! The one way tilth runs git.
//!
//! Every git command is a [`Git`]: it runs in the directory it is given,
//! pins the settings tilth's parsers depend on, and reads nothing from
//! stdin. Options are `&'static str`, so only text tilth wrote can become
//! one. A revision or path from anywhere else goes after `--end-of-options`
//! or `--`, so git never reads it as an option: `--output=<file>` would
//! otherwise make git overwrite that file (#277).
//!
//! A failure is a [`GitError`]. Its text is git's own words after the
//! command that failed. Nothing reads meaning out of git's stderr, because
//! git translates it; the variants come from what was asked and how git
//! exited.

use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Settings pinned on every command. User config can otherwise quote
/// non-ASCII paths or blank out empty context lines, and the diff parser
/// misreads either as a wrong path or a changed line (#208).
const PINNED: [&str; 4] = [
    "-c",
    "core.quotePath=false",
    "-c",
    "diff.suppressBlankEmpty=false",
];

/// How often a command with a deadline checks whether git has exited.
const POLL: Duration = Duration::from_millis(5);

/// One git command, built up and then run.
#[derive(Debug)]
#[must_use]
pub(crate) struct Git<'a> {
    dir: &'a Path,
    command: &'static str,
    args: Vec<&'static str>,
    revisions: Vec<&'a str>,
    paths: Vec<&'a Path>,
    also_success: Option<i32>,
    deadline: Option<Instant>,
}

impl<'a> Git<'a> {
    /// `git <command>`, run in `dir`.
    pub(crate) fn new(dir: &'a Path, command: &'static str) -> Self {
        Self {
            dir,
            command,
            args: Vec::new(),
            revisions: Vec::new(),
            paths: Vec::new(),
            also_success: None,
            deadline: None,
        }
    }

    /// Options and other arguments tilth writes itself.
    pub(crate) fn args(mut self, args: &[&'static str]) -> Self {
        self.args.extend_from_slice(args);
        self
    }

    /// A revision, range or `<rev>:<path>` spec, passed after
    /// `--end-of-options`.
    pub(crate) fn revision(mut self, revision: &'a str) -> Self {
        self.revisions.push(revision);
        self
    }

    /// A path, passed after `--`.
    pub(crate) fn path(mut self, path: &'a Path) -> Self {
        self.paths.push(path);
        self
    }

    /// Read exit code `code` as success too. `git diff --no-index` exits 1
    /// when the files differ.
    pub(crate) fn success_on(mut self, code: i32) -> Self {
        self.also_success = Some(code);
        self
    }

    /// Stop git and fail with [`GitError::TimedOut`] if it has not exited
    /// by `deadline`. Several commands can share one deadline, so together
    /// they take no longer than it allows.
    pub(crate) fn deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Run git and return what it printed on stdout.
    pub(crate) fn run(&self) -> Result<String, GitError> {
        let command = self.command;
        let io = |source| GitError::Io { command, source };
        let child = self.to_command().spawn().map_err(io)?;
        let output = match self.deadline {
            None => child.wait_with_output().map_err(io)?,
            Some(deadline) => wait_until(child, deadline)
                .map_err(io)?
                .ok_or(GitError::TimedOut { command })?,
        };
        if !(output.status.success() || output.status.code() == self.also_success) {
            return Err(GitError::Failed {
                command,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn to_command(&self) -> Command {
        let mut cmd = Command::new("git");
        // tilth only reads, so git skips the locks it takes only to write
        // back a refreshed index. A `git status` stopped by a time limit
        // would otherwise leave `.git/index.lock` behind (#357). The
        // variable, unlike `--no-optional-locks`, is ignored by a git
        // older than 2.15 instead of failing the command.
        cmd.env("GIT_OPTIONAL_LOCKS", "0")
            .current_dir(self.dir)
            .args(PINNED)
            .arg(self.command)
            .args(&self.args);
        if !self.revisions.is_empty() {
            cmd.arg("--end-of-options").args(&self.revisions);
        }
        if !self.paths.is_empty() {
            cmd.arg("--").args(&self.paths);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
}

/// Wait for `child` until `deadline`, and return what it printed. `None`
/// when it was still running and has been stopped.
///
/// Its output is read while it runs. A pipe holds only 64 KiB, and git waits
/// on a full pipe, so a git that printed more would otherwise never exit
/// before the deadline (#356).
fn wait_until(mut child: Child, deadline: Instant) -> io::Result<Option<Output>> {
    let readers = child
        .stdout
        .take()
        .map(drain)
        .transpose()
        .and_then(|stdout| {
            let stderr = child.stderr.take().map(drain).transpose()?;
            Ok((stdout, stderr))
        });
    let (stdout, stderr) = match readers {
        Ok(readers) => readers,
        Err(e) => {
            stop(&mut child);
            return Err(e);
        }
    };
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(Output {
                status,
                stdout: drained(stdout)?,
                stderr: drained(stderr)?,
            }));
        }
        if Instant::now() >= deadline {
            // The readers are not joined: each ends when its pipe closes,
            // and a process git started may hold a pipe open past the
            // deadline.
            stop(&mut child);
            return Ok(None);
        }
        thread::sleep(POLL);
    }
}

/// Kill `child` and reap it. Kill fails only if it exited in the meantime,
/// and the wait then just reaps it, so neither result changes the outcome.
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// A thread that reads `pipe` to its end.
fn drain(mut pipe: impl Read + Send + 'static) -> io::Result<JoinHandle<io::Result<Vec<u8>>>> {
    thread::Builder::new().spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    })
}

/// What a [`drain`] thread read, once its pipe has closed.
fn drained(reader: Option<JoinHandle<io::Result<Vec<u8>>>>) -> io::Result<Vec<u8>> {
    reader.map_or_else(
        || Ok(Vec::new()),
        |reader| {
            reader
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        },
    )
}

/// The top of the work tree `dir` is in. Git prints every diff path
/// relative to it, wherever it runs.
pub(crate) fn toplevel(dir: &Path) -> Result<PathBuf, GitError> {
    match Git::new(dir, "rev-parse").args(&["--show-toplevel"]).run() {
        Ok(top) => Ok(PathBuf::from(top.trim())),
        // Git finds no work tree at or above `dir`: no repository, a bare
        // one, or one it refuses to trust.
        Err(GitError::Failed { stderr, .. }) => Err(GitError::NotARepository { stderr }),
        Err(e) => Err(e),
    }
}

/// The id of the empty tree, in the hash format of the repository at
/// `dir`.
pub(crate) fn empty_tree(dir: &Path) -> Result<String, GitError> {
    // stdin is empty, so this hashes an empty tree.
    Git::new(dir, "hash-object")
        .args(&["-t", "tree", "--stdin"])
        .run()
        .map(|id| id.trim().to_string())
}

/// The best common ancestor of `a` and `b`.
pub(crate) fn merge_base(dir: &Path, a: &str, b: &str) -> Result<String, GitError> {
    Git::new(dir, "merge-base")
        .revision(a)
        .revision(b)
        .run()
        .map(|id| id.trim().to_string())
}

/// Why a git command gave no result.
#[derive(Debug)]
pub(crate) enum GitError {
    /// Git could not be started or read. A missing git is this, with
    /// `source.kind()` `NotFound`.
    Io {
        command: &'static str,
        source: io::Error,
    },
    /// The directory is in no work tree, from [`toplevel`].
    NotARepository { stderr: String },
    /// Git exited with a failure; `stderr` is what it said.
    Failed {
        command: &'static str,
        stderr: String,
    },
    /// Git had not exited by its deadline and was stopped.
    TimedOut { command: &'static str },
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { command, source } => write!(f, "failed to run git {command}: {source}"),
            Self::NotARepository { stderr } => write!(f, "git rev-parse failed: {stderr}"),
            Self::Failed { command, stderr } => write!(f, "git {command} failed: {stderr}"),
            Self::TimedOut { command } => write!(f, "git {command} did not finish in time"),
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "by design: tests read the files they check directly"
)]
pub(crate) mod tests {
    use super::*;

    /// tilth only reads a repository, so git must never take the index lock
    /// for it: a `git status` stopped by the overview's time limit would
    /// leave `.git/index.lock` behind and break the user's next commit
    /// (#357).
    #[test]
    fn every_command_takes_no_optional_locks() {
        let cmd = Git::new(Path::new("."), "status").to_command();
        let locks = cmd
            .get_envs()
            .find(|(name, _)| *name == "GIT_OPTIONAL_LOCKS")
            .and_then(|(_, value)| value);
        assert_eq!(locks, Some(std::ffi::OsStr::new("0")));
    }

    /// What git itself prints on stderr for `args` in `dir`, so a test
    /// compares with git's words in whatever language git speaks here.
    fn git_stderr(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(!out.status.success(), "git {args:?} must fail");
        String::from_utf8_lossy(&out.stderr).trim().to_string()
    }

    /// What `git <args>` prints in `dir`, trimmed. Fixtures are built with
    /// git itself, outside the runner, so the runner is not testing itself.
    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// An empty repository.
    pub(crate) fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create tempdir");
        git(dir.path(), &["init", "-q"]);
        dir
    }

    /// A revision that starts with `-` still reaches git as a revision:
    /// `git diff --output=<file>` would truncate the file.
    #[test]
    fn a_revision_never_acts_as_an_option() {
        let repo = repo();
        let outside = tempfile::tempdir().expect("create tempdir");
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, "keep me\n").unwrap();
        let injected = format!("--output={}", victim.display());

        let result = Git::new(repo.path(), "diff").revision(&injected).run();

        assert!(
            matches!(
                result,
                Err(GitError::Failed {
                    command: "diff",
                    ..
                })
            ),
            "{result:?}"
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me\n");
    }

    /// A path goes after `--`, so it is read as a path whatever its shape.
    #[test]
    fn a_path_never_acts_as_an_option() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let a = dir.path().join("a.txt");
        let b = dir.path().join("--output=b.txt");
        std::fs::write(&a, "one\n").unwrap();
        std::fs::write(&b, "two\n").unwrap();

        let patch = Git::new(dir.path(), "diff")
            .args(&["--no-index"])
            .path(&a)
            .path(Path::new("--output=b.txt"))
            .success_on(1)
            .run()
            .unwrap();

        assert!(patch.contains("-one\n+two\n"), "{patch}");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "two\n");
    }

    #[test]
    fn outside_a_work_tree_is_not_a_repository() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let err = toplevel(dir.path()).unwrap_err();
        let said = git_stderr(dir.path(), &["rev-parse", "--show-toplevel"]);
        assert!(
            matches!(&err, GitError::NotARepository { stderr } if *stderr == said),
            "{err:?}"
        );
        assert_eq!(err.to_string(), format!("git rev-parse failed: {said}"));
    }

    #[test]
    fn a_failed_command_carries_git_s_own_words() {
        let repo = repo();
        let err = merge_base(repo.path(), "no-such-ref", "HEAD").unwrap_err();
        let said = git_stderr(
            repo.path(),
            &["merge-base", "--end-of-options", "no-such-ref", "HEAD"],
        );
        assert!(
            matches!(&err, GitError::Failed { command: "merge-base", stderr } if *stderr == said),
            "{err:?}"
        );
        assert_eq!(err.to_string(), format!("git merge-base failed: {said}"));
    }

    /// Exit code 1 fails unless the command reads it as success.
    #[test]
    fn exit_one_is_success_only_when_asked() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, "one\n").unwrap();
        std::fs::write(&b, "two\n").unwrap();
        let differ = || {
            Git::new(dir.path(), "diff")
                .args(&["--no-index"])
                .path(&a)
                .path(&b)
        };
        assert!(matches!(differ().run(), Err(GitError::Failed { .. })));
        assert!(differ().success_on(1).run().is_ok());
    }

    /// Git cannot exit before a deadline that has already passed, so it is
    /// stopped.
    #[test]
    fn a_command_past_its_deadline_is_stopped() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let err = Git::new(dir.path(), "version")
            .deadline(Instant::now())
            .run()
            .unwrap_err();
        assert!(
            matches!(err, GitError::TimedOut { command: "version" }),
            "{err:?}"
        );
        assert_eq!(err.to_string(), "git version did not finish in time");
    }

    /// Before its deadline, a command gives the same result as without one.
    #[test]
    fn a_command_within_its_deadline_returns_its_output() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let bounded = Git::new(dir.path(), "version")
            .deadline(Instant::now() + Duration::from_secs(60))
            .run()
            .unwrap();
        let unbounded = Git::new(dir.path(), "version").run().unwrap();
        assert!(bounded.starts_with("git version "), "{bounded}");
        assert_eq!(bounded, unbounded);
    }

    /// Output larger than a pipe holds is read while git runs, so git can
    /// finish writing it and exit (#356).
    #[test]
    fn output_longer_than_a_pipe_arrives_before_the_deadline() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "line\n".repeat(32 * 1024)).unwrap();
        let patch = Git::new(dir.path(), "diff")
            .args(&["--no-index"])
            .path(&a)
            .path(&b)
            .success_on(1)
            .deadline(Instant::now() + Duration::from_secs(60))
            .run()
            .unwrap();
        assert!(patch.len() > 128 * 1024, "{} bytes", patch.len());
    }
}
