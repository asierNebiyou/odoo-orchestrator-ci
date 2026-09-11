//! Minimal, Odoo-agnostic git integration — task 3.2's primitive. Reads the
//! current branch of a real git working directory via the real `git` CLI
//! (`git -C <path> rev-parse --abbrev-ref HEAD`) and nothing more: no
//! cloning, no remotes, no commit history, no staged-changes detection.
//! Generic on purpose, the same posture as `pg_admin.rs`/`filestore.rs` —
//! this module doesn't know or assume the repository it's pointed at is an
//! Odoo addons repo, and it never mutates the repo it reads.
//!
//! **Deliberately uses `-C <path>` rather than requiring `path` to be a
//! repo's root.** A registered addons source's `path_or_url` is typically a
//! subdirectory of a larger repo (e.g. `~/repos/acme-odoo/addons`, the
//! example already used elsewhere in this codebase's own tests) — `git`
//! itself walks up parent directories to find the enclosing repo from any
//! subdirectory, exactly like running `git status` from inside `src/`
//! works, so this module doesn't need (and shouldn't add) its own
//! repo-root-finding logic.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("{0:?} is not inside a git working directory")]
    NotARepo(PathBuf),
    #[error("{0:?} is on a detached HEAD, not a named branch")]
    DetachedHead(PathBuf),
    #[error("{0:?} has no commits yet, so it has no resolvable branch")]
    NoCommitsYet(PathBuf),
    #[error("couldn't run git: {0}")]
    Spawn(std::io::Error),
    #[error("git exited with {exit_code:?}: {stderr}")]
    CommandFailed { exit_code: Option<i32>, stderr: String },
}

/// The current branch name for whatever git repository contains `repo_path`
/// (`repo_path` itself need not be the repo's root). Fails distinctly for
/// each real way this can go wrong rather than collapsing them into one
/// generic error, since the caller (`Core::current_git_branch`) treats
/// "nothing to suggest" very differently from "something is actually
/// broken here."
pub fn current_branch(repo_path: &Path) -> Result<String, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .output()
        .map_err(GitError::Spawn)?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.contains("not a git repository") {
            return Err(GitError::NotARepo(repo_path.to_path_buf()));
        }
        // A brand-new repo with zero commits: HEAD is a symbolic ref to an
        // unborn branch, so `rev-parse HEAD` itself fails even though
        // `--abbrev-ref` still happens to print the ref name to stdout
        // alongside the failure — real, observed `git` behavior, not a
        // guess, which is exactly why this checks the exit code rather
        // than trusting stdout on its own.
        if stderr.contains("ambiguous argument 'HEAD'") {
            return Err(GitError::NoCommitsYet(repo_path.to_path_buf()));
        }
        return Err(GitError::CommandFailed { exit_code: output.status.code(), stderr });
    }

    // On a real branch, `--abbrev-ref HEAD` prints the branch name. On a
    // detached HEAD (checked out to a bare commit/tag), it prints the
    // literal string "HEAD" instead — the one case where a *successful*
    // command still doesn't have a branch name to hand back.
    if stdout == "HEAD" {
        return Err(GitError::DetachedHead(repo_path.to_path_buf()));
    }

    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Runs `git` the same way `current_branch` does, for test setup only —
    /// initializes a repo, makes it a fully valid non-empty non-detached
    /// working tree, and returns the temp dir. `initial_branch` exercises
    /// that this module doesn't hardcode an assumption about "main" vs
    /// "master" vs anything else.
    fn real_git_repo(initial_branch: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-q", "-b", initial_branch]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "test"]);
        std::fs::write(dir.path().join("f"), b"hello").unwrap();
        run_git(dir.path(), &["add", "f"]);
        run_git(dir.path(), &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"]);
        dir
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git").arg("-C").arg(dir).args(args).status().expect("git should run");
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    #[test]
    fn reports_the_real_current_branch_name() {
        let repo = real_git_repo("main");
        assert_eq!(current_branch(repo.path()).unwrap(), "main");
    }

    #[test]
    fn does_not_hardcode_an_assumed_default_branch_name() {
        let repo = real_git_repo("trunk");
        assert_eq!(current_branch(repo.path()).unwrap(), "trunk");
    }

    #[test]
    fn tracks_a_real_branch_switch() {
        let repo = real_git_repo("main");
        run_git(repo.path(), &["checkout", "-q", "-b", "feature/new-invoicing"]);
        assert_eq!(current_branch(repo.path()).unwrap(), "feature/new-invoicing");
    }

    /// The realistic shape: a registered addons source points at a
    /// subdirectory of the actual repo (mirrors this codebase's own
    /// `~/repos/acme-odoo/addons` example elsewhere), not the repo root.
    #[test]
    fn finds_the_enclosing_repo_from_a_subdirectory_not_just_the_root() {
        let repo = real_git_repo("main");
        let addons_dir = repo.path().join("addons");
        std::fs::create_dir_all(&addons_dir).unwrap();
        assert_eq!(current_branch(&addons_dir).unwrap(), "main");
    }

    #[test]
    fn rejects_a_directory_that_is_not_a_git_repo_at_all() {
        let dir = TempDir::new().unwrap();
        let err = current_branch(dir.path()).unwrap_err();
        assert!(matches!(err, GitError::NotARepo(_)));
    }

    #[test]
    fn rejects_a_repo_with_no_commits_yet() {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        let err = current_branch(dir.path()).unwrap_err();
        assert!(matches!(err, GitError::NoCommitsYet(_)));
    }

    #[test]
    fn rejects_a_detached_head() {
        let repo = real_git_repo("main");
        let sha_output = Command::new("git").arg("-C").arg(repo.path()).args(["rev-parse", "HEAD"]).output().unwrap();
        let sha = String::from_utf8_lossy(&sha_output.stdout).trim().to_string();
        run_git(repo.path(), &["checkout", "-q", &sha]);
        let err = current_branch(repo.path()).unwrap_err();
        assert!(matches!(err, GitError::DetachedHead(_)));
    }
}

/// Clone `url` into `dest`, shallowly. Used for "my modules are on
/// GitHub" — the whole history of an addons repo is not interesting to a
/// tool that only reads manifests, and a shallow clone of a large repo is
/// the difference between seconds and minutes.
///
/// Authentication is deliberately whatever the machine already has: git's
/// own credential helper, an `ssh` key for `git@` URLs, or `gh auth`'s
/// helper if it's installed. This never prompts for or stores a token
/// itself — a desktop tool that asks for your GitHub password is one that
/// shouldn't be trusted with it.
pub fn clone(url: &str, dest: &Path) -> Result<PathBuf, GitError> {
    if dest.join(".git").is_dir() {
        return Ok(dest.to_path_buf()); // already cloned; leave it alone
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(GitError::Spawn)?;
    }
    let output = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg(url)
        .arg(dest)
        .output()
        .map_err(GitError::Spawn)?;
    if !output.status.success() {
        // A half-written clone would otherwise be mistaken for a good one
        // by the `.git` check above on the next attempt.
        let _ = std::fs::remove_dir_all(dest);
        return Err(GitError::CommandFailed {
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(dest.to_path_buf())
}
