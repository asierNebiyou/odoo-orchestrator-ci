//! Provisions an app-owned Python runtime and virtualenv via `uv`, per
//! `odoo-orchestrator-runtime-architecture.md`'s decision (uv, not a system
//! Python) — this is what task 2.2 needs before it can ever spawn
//! `odoo-bin`: a specific Python version, isolated per Odoo version, with
//! Odoo's actual dependencies installed into it.
//!
//! Applies the two Odoo-specific landmines the research doc found in Odoo's
//! own `requirements.txt`: `psycopg2` (what Odoo pins) has no Linux/macOS
//! PyPI wheels at all, so it's substituted with `psycopg2-binary`; and
//! `python-ldap` has no wheels anywhere (sdist only, needs OpenLDAP
//! headers), so it's dropped on non-Windows rather than requiring a
//! compiler toolchain for a no-compiler install.
//!
//! Verified against real `uv` and real PyPI, not mocked — see this module's
//! own tests: a real `uv python install` into an app-owned directory (not
//! wherever `uv` would otherwise put it), a real `uv venv`, and a real `uv
//! pip install psycopg2-binary` followed by actually importing it in the
//! provisioned interpreter.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tokio::task::spawn_blocking;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("couldn't run {command}: {source}")]
    Spawn { command: String, source: std::io::Error },
    #[error("{command} exited with {exit_code:?}: {stderr}")]
    CommandFailed { command: String, exit_code: Option<i32>, stderr: String },
}

/// Where to find `uv` and where its app-owned state should live. In
/// production `uv_bin` points at the bundled binary (per the runtime-
/// architecture doc: "vendor the binary directly"); `install_dir` is an
/// app-owned directory so provisioned Python versions don't leak into (or
/// depend on) anything already on the user's machine — deliberately not
/// wherever `uv python install` would put it by default.
#[derive(Debug, Clone)]
pub struct PythonRuntimeConfig {
    pub uv_bin: PathBuf,
    pub install_dir: PathBuf,
}

/// Ensures a standalone CPython `python_version` (e.g. `"3.11"`) is
/// available under this config's `install_dir`, downloading it via `uv
/// python install` if it isn't already. Idempotent — `uv` itself no-ops a
/// second install of the same version into the same install dir.
pub async fn ensure_python_installed(config: &PythonRuntimeConfig, python_version: &str) -> Result<(), RuntimeError> {
    let config = config.clone();
    let python_version = python_version.to_string();
    spawn_blocking(move || {
        std::fs::create_dir_all(&config.install_dir).ok();
        let mut cmd = Command::new(&config.uv_bin);
        cmd.arg("python").arg("install").arg(&python_version);
        cmd.env("UV_PYTHON_INSTALL_DIR", &config.install_dir);
        let output = run(cmd, "uv python install")?;
        check_success(&output, "uv python install")
    })
    .await
    .expect("uv python install blocking task panicked")
}

/// Ensures a venv exists at `venv_dir` using `python_version` (resolved from
/// this config's app-owned `install_dir`, not any system Python), returning
/// the venv's `python` executable path. Idempotent — skips creating a new
/// venv if one already looks present.
pub async fn ensure_venv(config: &PythonRuntimeConfig, python_version: &str, venv_dir: &Path) -> Result<PathBuf, RuntimeError> {
    let venv_python = venv_python_path(venv_dir);
    if venv_python.is_file() {
        return Ok(venv_python);
    }
    let config = config.clone();
    let python_version = python_version.to_string();
    let venv_dir_owned = venv_dir.to_path_buf();
    let venv_dir_for_task = venv_dir_owned.clone();
    spawn_blocking(move || {
        let mut cmd = Command::new(&config.uv_bin);
        cmd.arg("venv").arg("--python").arg(&python_version).arg(&venv_dir_for_task);
        cmd.env("UV_PYTHON_INSTALL_DIR", &config.install_dir);
        let output = run(cmd, "uv venv")?;
        check_success(&output, "uv venv")
    })
    .await
    .expect("uv venv blocking task panicked")?;
    Ok(venv_python_path(&venv_dir_owned))
}

fn venv_python_path(venv_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python")
    }
}

/// Installs `requirements` (plain `pip install`-style specifiers, e.g.
/// `"psycopg2==2.9.9"`) into the venv at `venv_python`, after applying the
/// platform adjustments in `adjust_requirements_for_this_platform` below.
pub async fn install_requirements(config: &PythonRuntimeConfig, venv_python: &Path, requirements: &[String]) -> Result<(), RuntimeError> {
    let adjusted = adjust_requirements_for_this_platform(requirements);
    if adjusted.is_empty() {
        return Ok(());
    }
    let config = config.clone();
    let venv_python = venv_python.to_path_buf();
    spawn_blocking(move || {
        let mut cmd = Command::new(&config.uv_bin);
        cmd.arg("pip").arg("install").arg("--python").arg(&venv_python);
        cmd.args(&adjusted);
        let output = run(cmd, "uv pip install")?;
        check_success(&output, "uv pip install")
    })
    .await
    .expect("uv pip install blocking task panicked")
}

/// Pure logic, deliberately separated from the network-touching install
/// call above so it can be unit-tested without `uv` or network access. See
/// this module's doc comment for why these two specific substitutions exist.
pub fn adjust_requirements_for_this_platform(requirements: &[String]) -> Vec<String> {
    requirements
        .iter()
        .filter_map(|req| {
            let name = requirement_name(req);
            if name.eq_ignore_ascii_case("python-ldap") && !cfg!(windows) {
                return None; // no wheels anywhere; Odoo itself already excludes it on Windows
            }
            if name.eq_ignore_ascii_case("psycopg2") {
                return Some("psycopg2-binary".to_string()); // psycopg2 has no Linux/macOS wheels
            }
            Some(req.clone())
        })
        .collect()
}

/// A requirement specifier's name is everything before the first
/// version/extras/marker operator — `"psycopg2==2.9.9"`, `"psycopg2>=2.9"`,
/// `"psycopg2[extra]"`, and plain `"psycopg2"` should all resolve to
/// `"psycopg2"`.
fn requirement_name(req: &str) -> &str {
    req.split(|c: char| "=<>!~[; ".contains(c)).next().unwrap_or(req).trim()
}

fn run(mut cmd: Command, name: &str) -> Result<Output, RuntimeError> {
    cmd.output().map_err(|e| RuntimeError::Spawn { command: name.to_string(), source: e })
}

fn check_success(output: &Output, name: &str) -> Result<(), RuntimeError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(RuntimeError::CommandFailed {
            command: name.to_string(),
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn discover_uv_bin() -> PathBuf {
        for dir in std::env::split_paths(&std::env::var("PATH").unwrap_or_default()) {
            let candidate = dir.join("uv");
            if candidate.is_file() {
                return candidate;
            }
        }
        // uv commonly installs to ~/.local/bin, which isn't always on PATH
        // in a non-interactive shell.
        let home_local = std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/bin/uv"));
        if let Some(p) = home_local {
            if p.is_file() {
                return p;
            }
        }
        panic!("these tests need `uv` installed — they exercise real provisioning, not a mock");
    }

    // --- pure logic: no network, no uv ------------------------------------

    #[test]
    fn substitutes_psycopg2_for_psycopg2_binary() {
        let out = adjust_requirements_for_this_platform(&["psycopg2==2.9.9".to_string()]);
        assert_eq!(out, vec!["psycopg2-binary".to_string()]);
    }

    #[test]
    fn substitutes_bare_psycopg2_with_no_version_pin() {
        let out = adjust_requirements_for_this_platform(&["psycopg2".to_string()]);
        assert_eq!(out, vec!["psycopg2-binary".to_string()]);
    }

    #[test]
    fn drops_python_ldap_on_non_windows() {
        let out = adjust_requirements_for_this_platform(&["python-ldap==3.4.3".to_string()]);
        assert!(out.is_empty(), "python-ldap should be dropped entirely on this platform");
    }

    #[test]
    fn leaves_unrelated_requirements_untouched() {
        let reqs = vec!["Babel==2.9.1".to_string(), "lxml>=4.6.0".to_string(), "Werkzeug".to_string()];
        assert_eq!(adjust_requirements_for_this_platform(&reqs), reqs);
    }

    #[test]
    fn handles_a_realistic_odoo_requirements_list() {
        let reqs = vec![
            "Babel==2.9.1".to_string(),
            "psycopg2==2.9.9".to_string(),
            "python-ldap==3.4.3".to_string(),
            "gevent==22.10.2".to_string(),
        ];
        let out = adjust_requirements_for_this_platform(&reqs);
        assert_eq!(out, vec!["Babel==2.9.1".to_string(), "psycopg2-binary".to_string(), "gevent==22.10.2".to_string()]);
    }

    // --- real uv, real PyPI, real interpreter ------------------------------

    #[tokio::test]
    async fn provisions_a_real_python_venv_and_installs_psycopg2_binary() {
        let install_dir = TempDir::new().unwrap();
        let venv_dir = TempDir::new().unwrap();
        let config = PythonRuntimeConfig { uv_bin: discover_uv_bin(), install_dir: install_dir.path().to_path_buf() };

        ensure_python_installed(&config, "3.11").await.expect("uv should download a standalone Python 3.11");

        let venv_python = ensure_venv(&config, "3.11", venv_dir.path()).await.expect("uv venv should succeed");
        assert!(venv_python.is_file(), "expected a python executable at {}", venv_python.display());

        // The actual claim this module makes: asking for "psycopg2" (what
        // Odoo pins) results in a real, importable psycopg2-binary install,
        // not a build failure from the source-only real psycopg2.
        install_requirements(&config, &venv_python, &["psycopg2==2.9.9".to_string()])
            .await
            .expect("installing psycopg2 should transparently install psycopg2-binary instead");

        let output = std::process::Command::new(&venv_python)
            .arg("-c")
            .arg("import psycopg2; print(psycopg2.__version__)")
            .output()
            .expect("running the provisioned interpreter should work");
        assert!(output.status.success(), "expected psycopg2 to import cleanly: {}", String::from_utf8_lossy(&output.stderr));
        let printed = String::from_utf8_lossy(&output.stdout);
        assert!(!printed.trim().is_empty(), "psycopg2.__version__ should print something");
    }

    #[tokio::test]
    async fn ensure_venv_is_idempotent() {
        let install_dir = TempDir::new().unwrap();
        let venv_dir = TempDir::new().unwrap();
        let config = PythonRuntimeConfig { uv_bin: discover_uv_bin(), install_dir: install_dir.path().to_path_buf() };

        ensure_python_installed(&config, "3.11").await.unwrap();
        let first = ensure_venv(&config, "3.11", venv_dir.path()).await.unwrap();
        let second = ensure_venv(&config, "3.11", venv_dir.path()).await.unwrap();
        assert_eq!(first, second, "second call should recognize the existing venv and skip creating it again");
    }
}
