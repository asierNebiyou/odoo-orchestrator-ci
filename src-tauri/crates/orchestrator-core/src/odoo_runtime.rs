//! Getting a usable Odoo of a given version, without asking the user where
//! it is.
//!
//! The rule, in order:
//!
//!   1. **Cache.** If this app already fetched that version, reuse it. One
//!      copy of Odoo 17 serves every project that wants Odoo 17 — the
//!      addons path is what makes two projects differ, not the framework.
//!   2. **Something already on this machine.** People who have been doing
//!      Odoo work for years already have checkouts lying around. Re-cloning
//!      a gigabyte when `~/odoo-dev` is right there is rude, so a checkout
//!      is adopted if it really is Odoo *and* really is the right series —
//!      both read out of its own `odoo/release.py`, never guessed from the
//!      folder's name.
//!   3. **Fetch it.** A shallow, single-branch clone of that series.
//!
//! Resolution (1 and 2) is deliberately separated from fetching (3) so the
//! decision logic is unit-testable with fixture directories and no network.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;

#[derive(Debug, thiserror::Error)]
pub enum OdooRuntimeError {
    #[error("couldn't run {command}: {source}")]
    Spawn { command: String, source: std::io::Error },
    #[error("{command} failed: {stderr}")]
    CommandFailed { command: String, stderr: String },
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{path} looks like Odoo {found}, not {wanted}")]
    WrongVersion { path: String, found: String, wanted: String },
    #[error("{path} isn't a copy this app downloaded, so it isn't this app's to delete")]
    NotInCache { path: String },
}

/// Where a usable Odoo of some version actually is. `venv_python` is
/// `None` until its dependencies have been installed — a checkout without
/// an interpreter can be found but not started, and callers have to deal
/// with that difference rather than discovering it as a crash later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OdooRuntime {
    pub version: String,
    pub checkout_root: PathBuf,
    pub venv_python: Option<PathBuf>,
    /// How this was obtained — worth surfacing, because "reused the
    /// checkout you already had at ~/odoo-dev" and "downloaded 1.1 GB" are
    /// very different things to have happened.
    pub source: RuntimeSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSource {
    /// Already fetched by this app for an earlier project.
    Cache,
    /// A checkout that was already on this machine.
    Adopted,
    /// Freshly downloaded.
    Fetched,
}

/// The app-owned folder holding one subdirectory per Odoo version, plus
/// the places to look before downloading anything.
#[derive(Debug, Clone)]
pub struct RuntimeStore {
    pub root: PathBuf,
    /// Directories that might already contain an Odoo checkout. Each is
    /// checked itself and one level down, so passing a home directory finds
    /// `~/odoo-dev` without needing every candidate spelled out.
    pub search_paths: Vec<PathBuf>,
}

impl RuntimeStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), search_paths: Vec::new() }
    }

    pub fn searching(mut self, paths: Vec<PathBuf>) -> Self {
        self.search_paths = paths;
        self
    }

    pub fn cache_dir(&self, version: &str) -> PathBuf {
        self.root.join(version)
    }

    /// Cache, then the machine — no network, no side effects. `None` means
    /// nothing usable was found and a fetch is the only way forward.
    pub fn resolve(&self, version: &str) -> Option<OdooRuntime> {
        let cached = self.cache_dir(version);
        if is_odoo_checkout(&cached) {
            return Some(OdooRuntime {
                version: version.to_string(),
                venv_python: venv_python_in(&cached),
                checkout_root: cached,
                source: RuntimeSource::Cache,
            });
        }

        for base in &self.search_paths {
            for candidate in candidates_under(base) {
                if !is_odoo_checkout(&candidate) {
                    continue;
                }
                // The folder's name means nothing — a directory called
                // "odoo-17" can hold 16.0, and frequently does.
                match series_of(&candidate) {
                    Some(found) if found == version => {
                        return Some(OdooRuntime {
                            version: version.to_string(),
                            venv_python: venv_python_in(&candidate),
                            checkout_root: candidate,
                            source: RuntimeSource::Adopted,
                        })
                    }
                    _ => continue,
                }
            }
        }
        None
    }

    /// Resolve, or clone that series if nothing was found. The clone is
    /// shallow and single-branch: a full Odoo history is several gigabytes
    /// and nothing here needs it.
    pub async fn ensure(&self, version: &str) -> Result<OdooRuntime, OdooRuntimeError> {
        if let Some(found) = self.resolve(version) {
            return Ok(found);
        }
        let target = self.cache_dir(version);
        let version = version.to_string();
        let cloned = spawn_blocking({
            let target = target.clone();
            let version = version.clone();
            move || clone_odoo(&version, &target)
        })
        .await
        .expect("odoo clone task panicked")?;

        Ok(OdooRuntime {
            version,
            venv_python: venv_python_in(&cloned),
            checkout_root: cloned,
            source: RuntimeSource::Fetched,
        })
    }

    /// Adopt a checkout the user pointed at explicitly. Same version check
    /// as discovery — pointing this at the wrong series is a mistake worth
    /// catching now rather than as a confusing failure at first start.
    pub fn adopt(&self, version: &str, path: impl AsRef<Path>) -> Result<OdooRuntime, OdooRuntimeError> {
        let path = path.as_ref().to_path_buf();
        let found = series_of(&path).unwrap_or_else(|| "something that isn't Odoo".to_string());
        if found != version {
            return Err(OdooRuntimeError::WrongVersion {
                path: path.display().to_string(),
                found,
                wanted: version.to_string(),
            });
        }
        Ok(OdooRuntime {
            version: version.to_string(),
            venv_python: venv_python_in(&path),
            checkout_root: path,
            source: RuntimeSource::Adopted,
        })
    }

    /// Every copy of Odoo this app has fetched, with what it actually
    /// weighs on disk. The cache is the single biggest surprise on a
    /// machine that's been used for a while — three unused copies of Odoo
    /// is easily 3 GB — so it has to be listable, not just resolvable.
    ///
    /// The version reported is read out of each copy's own `release.py`,
    /// not taken from the folder name, for the same reason `resolve` does
    /// it: a folder called `17.0` that holds 16.0 is a real thing that
    /// happens, and showing the lie would be worse than showing nothing.
    pub fn cached(&self) -> Vec<CachedRuntime> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_odoo_checkout(&path) {
                continue;
            }
            let folder = entry.file_name().to_string_lossy().to_string();
            out.push(CachedRuntime {
                version: series_of(&path).unwrap_or_else(|| folder.clone()),
                folder,
                size_bytes: directory_size(&path),
                has_venv: venv_python_in(&path).is_some(),
                path,
            });
        }
        out.sort_by(|a, b| a.version.cmp(&b.version));
        out
    }

    /// Delete one cached copy. Refuses anything that isn't directly inside
    /// this store's own root: adopted checkouts belong to the user and
    /// this app has no business deleting them, and "clear the cache"
    /// eating someone's `~/odoo-dev` would be unforgivable.
    pub fn remove_cached(&self, folder: &str) -> Result<u64, OdooRuntimeError> {
        let target = self.root.join(folder);
        let canonical_root = self.root.canonicalize()?;
        let canonical_target = target.canonicalize()?;
        if canonical_target.parent() != Some(canonical_root.as_path()) {
            return Err(OdooRuntimeError::NotInCache { path: target.display().to_string() });
        }
        let freed = directory_size(&canonical_target);
        std::fs::remove_dir_all(&canonical_target)?;
        Ok(freed)
    }
}

/// One copy of Odoo sitting in the app's own cache folder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedRuntime {
    /// The series it really is, read from `release.py`.
    pub version: String,
    /// The folder it's in, which is also the handle for deleting it.
    pub folder: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    /// Whether its dependencies are installed — a copy without an
    /// interpreter can be found but not started.
    pub has_venv: bool,
}

/// Bytes used by everything under `path`, following no symlinks and
/// counting hardlinked files once per link (which is what "how much would
/// deleting this folder free" means to someone looking at a snapshot's
/// hardlinked filestore — nothing, and the number should not pretend
/// otherwise… but `std::fs` gives no portable link count on all targets,
/// so this is deliberately the simple sum and the UI says "on disk",
/// not "reclaimable", for filestores).
///
/// Returns 0 for a path that doesn't exist, rather than erroring: a
/// missing backups folder is a legitimate state meaning "no backups yet",
/// not a failure worth propagating into a stats screen.
pub fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            total += directory_size(&entry.path());
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

fn clone_odoo(version: &str, target: &Path) -> Result<PathBuf, OdooRuntimeError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut cmd = Command::new("git");
    cmd.arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--single-branch")
        .arg("--branch")
        .arg(version)
        .arg("https://github.com/odoo/odoo.git")
        .arg(target);
    let output = cmd
        .output()
        .map_err(|source| OdooRuntimeError::Spawn { command: "git clone".into(), source })?;
    if !output.status.success() {
        // A failed clone can leave a partial directory behind, which the
        // cache check would then happily "resolve" to on the next call.
        let _ = std::fs::remove_dir_all(target);
        return Err(OdooRuntimeError::CommandFailed {
            command: "git clone".into(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(target.to_path_buf())
}

/// Odoo's own marker files. `odoo-bin` alone isn't enough — plenty of
/// wrapper repos ship one — so the package's `release.py` has to be there
/// too, which is also what the version is read from.
pub fn is_odoo_checkout(path: &Path) -> bool {
    path.join("odoo-bin").is_file() && path.join("odoo").join("release.py").is_file()
}

/// The series string (`"17.0"`) a checkout actually is, read from its own
/// `odoo/release.py`. Handles both the `version_info` tuple and the
/// `series`/`major_version` constants, because which of them is
/// authoritative has moved around across versions.
pub fn series_of(path: &Path) -> Option<String> {
    let release = std::fs::read_to_string(path.join("odoo").join("release.py")).ok()?;
    for line in release.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("version_info") {
            if let Some(open) = rest.find('(') {
                let inner = &rest[open + 1..];
                let mut parts = inner.split(',').map(str::trim);
                let major = parts.next()?.trim_matches('\'').trim_matches('"');
                let minor = parts.next()?.trim_matches('\'').trim_matches('"');
                // Saas builds carry a leading "saas~" on the major field;
                // they're a different series and shouldn't silently match.
                if major.starts_with("saas") {
                    return Some(format!("{major}.{minor}"));
                }
                if let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) {
                    return Some(format!("{major}.{minor}"));
                }
            }
        }
        for key in ["series", "major_version"] {
            if let Some(rest) = line.strip_prefix(key) {
                if let Some(eq) = rest.find('=') {
                    let value = rest[eq + 1..].trim().trim_matches('\'').trim_matches('"');
                    if !value.is_empty() && value.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }
    None
}

fn venv_python_in(checkout: &Path) -> Option<PathBuf> {
    for venv in [".venv", "venv"] {
        for bin in ["bin/python", "bin/python3", "Scripts/python.exe"] {
            let candidate = checkout.join(venv).join(bin);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// A base directory itself, plus its immediate children — enough to find
/// `~/odoo-dev` when handed `~`, without walking an entire home directory.
fn candidates_under(base: &Path) -> Vec<PathBuf> {
    let mut out = vec![base.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that looks exactly like a real Odoo checkout to the
    /// checks above: `odoo-bin` plus an `odoo/release.py` declaring a
    /// series.
    fn fake_checkout(dir: &Path, version_info: &str) {
        std::fs::create_dir_all(dir.join("odoo")).unwrap();
        std::fs::write(dir.join("odoo-bin"), "#!/usr/bin/env python3\n").unwrap();
        std::fs::write(
            dir.join("odoo").join("release.py"),
            format!("RELEASE_LEVELS = ['alpha']\nversion_info = {version_info}\n"),
        )
        .unwrap();
    }

    fn fake_venv(dir: &Path) {
        std::fs::create_dir_all(dir.join(".venv").join("bin")).unwrap();
        std::fs::write(dir.join(".venv").join("bin").join("python"), "").unwrap();
    }

    #[test]
    fn reads_the_series_out_of_a_checkout_rather_than_its_folder_name() {
        let dir = tempfile::TempDir::new().unwrap();
        // Deliberately misleading name — this is the whole point.
        let checkout = dir.path().join("odoo-17-latest");
        fake_checkout(&checkout, "(16, 0, 0, 'final', 0, '')");
        assert_eq!(series_of(&checkout).as_deref(), Some("16.0"));

        let saas = dir.path().join("saas");
        fake_checkout(&saas, "('saas~16', 4, 0, 'final', 0, '')");
        assert_eq!(series_of(&saas).as_deref(), Some("saas~16.4"));

        let plain = dir.path().join("plain");
        std::fs::create_dir_all(plain.join("odoo")).unwrap();
        std::fs::write(plain.join("odoo-bin"), "").unwrap();
        std::fs::write(plain.join("odoo").join("release.py"), "series = '18.0'\n").unwrap();
        assert_eq!(series_of(&plain).as_deref(), Some("18.0"));
    }

    #[test]
    fn a_cached_version_is_reused_and_reports_itself_as_cached() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = RuntimeStore::new(dir.path());
        assert!(store.resolve("17.0").is_none(), "nothing cached yet");

        fake_checkout(&store.cache_dir("17.0"), "(17, 0, 0, 'final', 0, '')");
        let found = store.resolve("17.0").expect("should now resolve from cache");
        assert_eq!(found.source, RuntimeSource::Cache);
        assert_eq!(found.checkout_root, store.cache_dir("17.0"));
        assert_eq!(found.venv_python, None, "a checkout with no venv can't be started yet");

        fake_venv(&store.cache_dir("17.0"));
        assert!(store.resolve("17.0").unwrap().venv_python.is_some());

        // A different version is still missing — the cache is per-series.
        assert!(store.resolve("16.0").is_none());
    }

    #[test]
    fn a_checkout_already_on_this_machine_is_adopted_instead_of_downloaded() {
        let home = tempfile::TempDir::new().unwrap();
        let cache = tempfile::TempDir::new().unwrap();

        // What a real machine looks like: several folders, only one of
        // which is the version being asked for, and one that isn't Odoo.
        fake_checkout(&home.path().join("odoo-dev"), "(16, 0, 0, 'final', 0, '')");
        fake_checkout(&home.path().join("erp"), "(17, 0, 0, 'final', 0, '')");
        std::fs::create_dir_all(home.path().join("notes")).unwrap();

        let store = RuntimeStore::new(cache.path()).searching(vec![home.path().to_path_buf()]);

        let found = store.resolve("17.0").expect("the 17.0 checkout should be adopted");
        assert_eq!(found.source, RuntimeSource::Adopted);
        assert_eq!(found.checkout_root, home.path().join("erp"));

        let older = store.resolve("16.0").expect("the 16.0 checkout should be adopted");
        assert_eq!(older.checkout_root, home.path().join("odoo-dev"));

        // Nothing on this machine is 18.0, so adoption must not invent one.
        assert!(store.resolve("18.0").is_none());
    }

    #[test]
    fn the_cache_wins_over_a_checkout_on_the_machine() {
        let home = tempfile::TempDir::new().unwrap();
        let cache = tempfile::TempDir::new().unwrap();
        fake_checkout(&home.path().join("odoo-dev"), "(17, 0, 0, 'final', 0, '')");
        let store = RuntimeStore::new(cache.path()).searching(vec![home.path().to_path_buf()]);
        fake_checkout(&store.cache_dir("17.0"), "(17, 0, 0, 'final', 0, '')");

        let found = store.resolve("17.0").unwrap();
        assert_eq!(found.source, RuntimeSource::Cache, "the app's own copy is preferred");
    }

    #[test]
    fn adopting_the_wrong_series_explicitly_is_refused_with_both_versions_named() {
        let dir = tempfile::TempDir::new().unwrap();
        let checkout = dir.path().join("mine");
        fake_checkout(&checkout, "(16, 0, 0, 'final', 0, '')");
        let store = RuntimeStore::new(dir.path().join("cache"));

        match store.adopt("17.0", &checkout) {
            Err(OdooRuntimeError::WrongVersion { found, wanted, .. }) => {
                assert_eq!(found, "16.0");
                assert_eq!(wanted, "17.0");
            }
            other => panic!("expected WrongVersion, got {other:?}"),
        }
        assert!(store.adopt("16.0", &checkout).is_ok());
    }

    #[test]
    fn a_directory_that_merely_has_odoo_bin_is_not_treated_as_odoo() {
        let dir = tempfile::TempDir::new().unwrap();
        let wrapper = dir.path().join("my-wrapper-repo");
        std::fs::create_dir_all(&wrapper).unwrap();
        std::fs::write(wrapper.join("odoo-bin"), "#!/bin/sh\nexec real-odoo \"$@\"\n").unwrap();

        assert!(!is_odoo_checkout(&wrapper));
        let store = RuntimeStore::new(dir.path().join("cache")).searching(vec![dir.path().to_path_buf()]);
        assert!(store.resolve("17.0").is_none());
    }
}
