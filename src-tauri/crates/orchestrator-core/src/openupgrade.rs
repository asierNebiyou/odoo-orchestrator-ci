//! Fetching OCA's OpenUpgrade, and knowing which hops a migration needs.
//!
//! OpenUpgrade is the only community path for a major-version Odoo
//! migration — Odoo SA's own upgrade service is the paid, hosted
//! alternative. It's distributed as one repository with **a branch per
//! target version**: branch `17.0` migrates a 16.0 database to 17.0.
//!
//! Two things about it shape everything here:
//!
//! * **You cannot skip a version.** Going 15.0 → 18.0 means running 16.0,
//!   then 17.0, then 18.0, in order. OCA's own documentation says so
//!   plainly, and each branch only knows how to come from the one before.
//! * **There is no wrapper around that chain.** Every hop is a manual
//!   `odoo-bin --update all` against a database you had better have backed
//!   up, and if hop three fails you are somewhere between two versions with
//!   whatever you remembered to copy beforehand. That missing wrapper —
//!   checkpoints, ordering, a rollback that works — is what this module and
//!   `Core::run_upgrade` are.
//!
//! Since 14.0 the branch ships two Odoo modules at its repository root,
//! `openupgrade_framework` and `openupgrade_scripts`, so the repository root
//! itself goes on the addons path. `openupgrade_framework` also has to be
//! loaded server-wide (`--load`), because it patches Odoo's own module
//! loading before the registry is built.
//!
//! Verified against the real repository: branches 5.0 through 19.0 exist,
//! and 17.0's root holds exactly those two modules and a `requirements.txt`
//! naming `openupgradelib` as its only dependency.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum OpenUpgradeError {
    #[error("couldn't run {command}: {source}")]
    Spawn { command: String, source: std::io::Error },
    #[error("{command} failed: {stderr}")]
    CommandFailed { command: String, stderr: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The versions OpenUpgrade can migrate *to*, newest first.
///
/// Hard-coded rather than read from the network: this list decides what the
/// UI offers, and a screen that shows nothing because GitHub was slow is
/// worse than one that offers a version and finds out at fetch time. It is
/// checked against the real repository's branch list — 5.0 through 19.0 —
/// and trimmed to the ones this app can actually run, which is the modern
/// (14.0+) layout where OpenUpgrade ships as two addons.
pub const MIGRATABLE_TO: [&str; 6] = ["19.0", "18.0", "17.0", "16.0", "15.0", "14.0"];

/// The hops needed to get from `from` to `to`, in order.
///
/// Returns `None` when the request makes no sense — going backwards, or to
/// a version OpenUpgrade has no branch for. An empty vector means "already
/// there", which is a valid answer and not an error.
pub fn hops(from: &str, to: &str) -> Option<Vec<String>> {
    let from_major = major_of(from)?;
    let to_major = major_of(to)?;
    if to_major < from_major {
        return None;
    }
    // Every intermediate version has to have a branch, not just the target:
    // a chain with a missing rung is not a chain.
    let chain: Vec<String> = ((from_major + 1)..=to_major).map(|major| format!("{major}.0")).collect();
    if chain.iter().any(|v| !MIGRATABLE_TO.contains(&v.as_str())) {
        return None;
    }
    Some(chain)
}

fn major_of(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

/// Where a checkout of OpenUpgrade's `version` branch lives in the cache.
pub fn cache_dir(root: &Path, version: &str) -> PathBuf {
    root.join("openupgrade").join(version)
}

/// A checkout of the branch for `version`, fetched if it isn't already
/// there. Shallow and single-branch, the same shape as the Odoo clone —
/// about 13 MB for 17.0.
pub fn ensure(root: &Path, version: &str) -> Result<PathBuf, OpenUpgradeError> {
    let target = cache_dir(root, version);
    if target.join("openupgrade_framework").join("__manifest__.py").is_file() {
        return Ok(target);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A previous failed attempt can leave a partial directory that would
    // otherwise be "resolved" above on the next call.
    let _ = std::fs::remove_dir_all(&target);

    let mut cmd = Command::new("git");
    cmd.arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--single-branch")
        .arg("--branch")
        .arg(version)
        // `blob:none` fetches file contents lazily. The migration scripts
        // for one version are a few hundred files out of many thousands, so
        // this is the difference between 13 MB and several hundred.
        .arg("--filter=blob:none")
        .arg("https://github.com/OCA/OpenUpgrade.git")
        .arg(&target);
    let output = cmd
        .output()
        .map_err(|source| OpenUpgradeError::Spawn { command: "git clone".into(), source })?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&target);
        return Err(OpenUpgradeError::CommandFailed {
            command: format!("git clone OpenUpgrade {version}"),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(target)
}

/// What one hop of a migration is, and whether it can actually be run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hop {
    pub from: String,
    pub to: String,
    /// Whether this app already has the Odoo for `to` on disk. `false`
    /// isn't a blocker — it's a download — but it is the difference between
    /// a migration that starts now and one that starts in ten minutes, and
    /// saying so beforehand is better than a spinner.
    pub odoo_ready: bool,
    pub openupgrade_ready: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_step_is_one_hop() {
        assert_eq!(hops("16.0", "17.0"), Some(vec!["17.0".to_string()]));
    }

    /// The rule that makes this feature necessary: OpenUpgrade cannot skip
    /// versions, so 15 → 18 is three separate migrations in order, each of
    /// which can fail on its own.
    #[test]
    fn skipping_versions_is_expanded_into_every_intermediate_hop() {
        assert_eq!(hops("15.0", "18.0"), Some(vec!["16.0".to_string(), "17.0".to_string(), "18.0".to_string()]));
    }

    #[test]
    fn already_there_is_no_hops_rather_than_an_error() {
        assert_eq!(hops("17.0", "17.0"), Some(vec![]));
    }

    /// Downgrading isn't a thing OpenUpgrade does, and pretending otherwise
    /// would offer a button that destroys a database.
    #[test]
    fn going_backwards_is_refused() {
        assert_eq!(hops("17.0", "16.0"), None);
    }

    /// A chain is only as good as its rungs: if any intermediate version
    /// has no branch, the whole chain is impossible, not partially possible.
    #[test]
    fn a_chain_through_a_version_with_no_branch_is_refused() {
        // 13.0 has a branch in the real repository but not the modern
        // layout this app can run, so it is deliberately not in the list.
        assert_eq!(hops("12.0", "15.0"), None);
        assert_eq!(hops("13.0", "14.0"), Some(vec!["14.0".to_string()]));
    }

    #[test]
    fn nonsense_versions_are_refused_rather_than_guessed() {
        assert_eq!(hops("not-a-version", "17.0"), None);
        assert_eq!(hops("16.0", "saas~17"), None);
    }
}
