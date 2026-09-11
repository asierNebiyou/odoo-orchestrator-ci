//! Scans every addons source configured for a server, parses each module's
//! manifest (`manifest.rs`), and resolves the result the way Odoo itself
//! would at startup: for any technical name that appears in more than one
//! source, the lowest-`rank` source wins and the rest are silently shadowed.
//! Odoo has no concept of this — it just picks whichever addons-path entry it
//! finds the module in first — so this is the detector for a real,
//! currently-invisible bug class (see `odoo-orchestrator-research-and-direction.md`).
//!
//! Nothing here is cached or persisted: a scan walks the filesystem fresh
//! every time it's asked to, which is the right tradeoff until there's
//! evidence addons-paths are large enough (thousands of modules) for that to
//! matter — see `odoo-orchestrator-task-breakdown.md` 0.3's "modules-cache"
//! note for where that would go if it ever does.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::manifest::{self, ManifestError, ModuleManifest};
use crate::model::{AddonsSource, SourceKind};

const MANIFEST_FILENAMES: [&str; 2] = ["__manifest__.py", "__openerp__.py"];

#[derive(Debug, Clone, Serialize)]
pub struct SourceScanError {
    pub source_id: Uuid,
    pub source_label: String,
    pub path_or_url: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestParseIssue {
    pub source_label: String,
    pub technical_name: String,
    pub path: PathBuf,
    pub message: String,
}

/// One module as seen from one source. Before shadowing is resolved there can
/// be several of these sharing a `technical_name`.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleListing {
    pub technical_name: String,
    pub name: String,
    pub version: Option<String>,
    pub category: Option<String>,
    pub application: bool,
    pub installable: bool,
    pub auto_install_enabled: bool,
    pub depends: Vec<String>,
    pub source_id: Uuid,
    pub source_label: String,
    pub source_kind: SourceKind,
    pub rank: u32,
    /// True when a lower-`rank` source also provides this technical name —
    /// this copy exists on disk but Odoo will never actually load it.
    pub shadowed: bool,
    /// Absolute path to this copy's module directory — the "show the
    /// underlying command/location" principle applies to scan results too;
    /// a shadowed copy is only actionable if you can see exactly which
    /// directory on disk it is.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShadowedCopy {
    pub source_label: String,
    pub source_kind: SourceKind,
    pub rank: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Collision {
    pub technical_name: String,
    pub winner_source_label: String,
    pub winner_rank: u32,
    pub shadowed: Vec<ShadowedCopy>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnresolvedDependency {
    pub technical_name: String,
    pub missing_dependency: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleScan {
    pub server_id: Uuid,
    pub scanned_at: DateTime<Utc>,
    /// Every module found in every source, winners and shadowed copies alike
    /// — the frontend groups these by source/provenance, same mental model
    /// as `Modules.dc.html`.
    pub modules: Vec<ModuleListing>,
    pub collisions: Vec<Collision>,
    /// Dependencies of a *winning* module that don't resolve to any other
    /// winning module in this scan. Not necessarily a real problem — it also
    /// fires when the scan simply doesn't include a core Odoo checkout — but
    /// worth surfacing rather than silently ignoring (per the "show your
    /// work" principle: a black box that quietly drops unresolvable deps is
    /// exactly what senior devs distrust).
    pub unresolved_depends: Vec<UnresolvedDependency>,
    /// Topological install order over the winning modules, when one exists.
    pub install_order: Option<Vec<String>>,
    /// Populated instead of `install_order` if the winning module set has an
    /// actual dependency cycle — a real bug in someone's manifests, not a
    /// scanner limitation.
    pub cycle: Option<Vec<String>>,
    pub source_errors: Vec<SourceScanError>,
    pub parse_errors: Vec<ManifestParseIssue>,
}

struct RawFind {
    technical_name: String,
    path: PathBuf,
    manifest: ModuleManifest,
}

/// One folder that could go on the addons path, and what this app thinks
/// it is.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DiscoveredRoot {
    pub path: PathBuf,
    /// A short name for the folder, taken from the repository it's in where
    /// that's obvious.
    pub label: String,
    pub kind: SourceKind,
    /// The modules directly inside it.
    pub module_count: usize,
    /// A few names, so a person can recognise the folder without opening it.
    pub sample: Vec<String>,
    /// Why this app thinks it's that kind — shown, because a guess a person
    /// can't check is a guess they have to trust.
    pub why: String,
}

/// Finds the folders under `root` that are addons directories, and guesses
/// what each one is.
///
/// A folder counts when it directly contains at least one module — a
/// subdirectory with a `__manifest__.py` in it. That's exactly Odoo's own
/// rule for an addons-path entry, which is why the check is that and not
/// something cleverer.
///
/// The classification is Doodba's priority model, generated rather than
/// hand-written: your own code first, then OCA, then Odoo's own. It is a
/// **proposal**, and every entry carries the reason so it can be argued
/// with.
pub fn discover_addons_roots(root: &Path, max_depth: usize) -> Vec<DiscoveredRoot> {
    let mut found = Vec::new();
    walk_for_addons(root, root, max_depth, &mut found);
    // Deepest-first would put `odoo/addons` before `addons`; sort by the
    // proposed load order instead, so the list reads as the addons path it
    // would become.
    found.sort_by(|a, b| kind_rank(a.kind).cmp(&kind_rank(b.kind)).then_with(|| a.path.cmp(&b.path)));
    found
}

fn kind_rank(kind: SourceKind) -> u8 {
    match kind {
        SourceKind::Private => 0,
        SourceKind::Oca => 1,
        SourceKind::Core => 2,
    }
}

fn walk_for_addons(root: &Path, dir: &Path, depth_left: usize, into: &mut Vec<DiscoveredRoot>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut children = Vec::new();
    let mut modules = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // `.git` and friends hold no modules, and walking `node_modules` or
        // a virtualenv is thousands of directories of pure waste.
        if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "__pycache__" | "venv" | ".venv" | "site-packages") {
            continue;
        }
        if MANIFEST_FILENAMES.iter().any(|m| path.join(m).is_file()) {
            modules.push(name);
        } else {
            children.push(path);
        }
    }

    if !modules.is_empty() {
        modules.sort();
        let (kind, why) = classify(root, dir, &modules);
        into.push(DiscoveredRoot {
            label: label_for(root, dir),
            path: dir.to_path_buf(),
            kind,
            module_count: modules.len(),
            sample: modules.iter().take(4).cloned().collect(),
            why,
        });
        // Deliberately keep walking: an Odoo checkout has modules in
        // `addons/` *and* in `odoo/addons/`, and stopping at the first hit
        // would miss the one holding `base`.
    }
    if depth_left == 0 {
        return;
    }
    for child in children {
        walk_for_addons(root, &child, depth_left - 1, into);
    }
}

/// Odoo's own addons live in a checkout that also has `odoo-bin`; OCA
/// repositories are named `oca/<something>` or sit under a folder called
/// OCA. Everything else is taken to be the user's own code, which is the
/// safe default — misfiling your own module as OCA would silently change
/// which copy wins a shadowing collision.
fn classify(root: &Path, dir: &Path, modules: &[String]) -> (SourceKind, String) {
    let in_odoo_checkout = dir
        .ancestors()
        .take(3)
        .any(|a| a.join("odoo-bin").is_file() || (a.file_name().is_some_and(|n| n == "odoo") && a.join("addons").is_dir()));
    if in_odoo_checkout {
        let which = if dir.ends_with("odoo/addons") { "Odoo's own base modules" } else { "Odoo's bundled addons" };
        return (SourceKind::Core, format!("inside an Odoo checkout — {which}"));
    }

    let path_text = dir.strip_prefix(root).unwrap_or(dir).to_string_lossy().to_lowercase();
    if path_text.split(['/', '\\']).any(|segment| segment == "oca" || segment.starts_with("oca-")) {
        return (SourceKind::Oca, "in a folder named OCA".to_string());
    }
    // OCA repositories conventionally hold a `setup/` directory with one
    // subdirectory per module. It's a strong, checkable signal.
    if dir.join("setup").is_dir() && modules.iter().any(|m| dir.join("setup").join(m).is_dir()) {
        return (SourceKind::Oca, "has OCA's setup/ layout".to_string());
    }
    (SourceKind::Private, "not inside an Odoo checkout, so treated as your own code".to_string())
}

fn label_for(root: &Path, dir: &Path) -> String {
    // The repository this folder is in, when it's in one — that's the name
    // a person actually calls it.
    for ancestor in dir.ancestors().take(4) {
        if ancestor.join(".git").exists() {
            if let Some(repo) = ancestor.file_name().and_then(|n| n.to_str()) {
                let relative = dir.strip_prefix(ancestor).unwrap_or(dir);
                return if relative.as_os_str().is_empty() {
                    repo.to_string()
                } else {
                    format!("{repo}/{}", relative.display())
                };
            }
        }
    }
    dir.strip_prefix(root)
        .ok()
        .filter(|r| !r.as_os_str().is_empty())
        .map(|r| r.display().to_string())
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| dir.display().to_string())
}

/// A hash of everything in one module folder that Odoo would load.
///
/// This is the "has this module actually changed" question, answered by
/// content rather than by mtime — a `git checkout` rewrites every mtime in
/// a repo while changing almost nothing, and updating forty modules
/// because a branch switch touched them is exactly the waste this avoids.
///
/// Both the path and the bytes of each file go in, so a rename registers
/// as a change. Files are visited in sorted order so the hash doesn't
/// depend on how the filesystem happens to enumerate them.
///
/// Skipped: `.pyc` and `__pycache__` (build output — they change when
/// Python decides, not when the developer does), and anything under a
/// dot-directory such as `.git`.
pub fn content_hash(module_dir: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut files = Vec::new();
    collect_hashable_files(module_dir, module_dir, &mut files);
    if files.is_empty() {
        return None;
    }
    files.sort();

    let mut hasher = Sha256::new();
    for relative in &files {
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        match fs::read(module_dir.join(relative)) {
            Ok(bytes) => hasher.update(&bytes),
            // A file that can't be read is still a fact about this module,
            // and one that must not hash the same as the file being absent.
            Err(_) => hasher.update(b"<unreadable>"),
        }
        hasher.update([0]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

fn collect_hashable_files(root: &Path, dir: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "__pycache__" {
            continue;
        }
        if path.is_dir() {
            collect_hashable_files(root, &path, into);
        } else if !name.ends_with(".pyc") {
            if let Ok(relative) = path.strip_prefix(root) {
                into.push(relative.to_path_buf());
            }
        }
    }
}

fn scan_source_dir(source: &AddonsSource) -> (Vec<RawFind>, Option<SourceScanError>, Vec<ManifestParseIssue>) {
    let dir = Path::new(&source.path_or_url);
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(err) => {
            return (
                Vec::new(),
                Some(SourceScanError {
                    source_id: source.id,
                    source_label: source.label.clone(),
                    path_or_url: source.path_or_url.clone(),
                    message: format!("couldn't read addons directory: {err}"),
                }),
                Vec::new(),
            );
        }
    };

    let mut found = Vec::new();
    let mut parse_errors = Vec::new();

    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(technical_name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        let manifest_path = MANIFEST_FILENAMES.iter().map(|f| path.join(f)).find(|p| p.is_file());
        let Some(manifest_path) = manifest_path else {
            continue; // not a module directory (no manifest) — skip quietly, this is normal
        };
        match manifest::parse_manifest_file(&manifest_path, &technical_name) {
            Ok(manifest) => found.push(RawFind { technical_name, path: path.clone(), manifest }),
            Err(err) => parse_errors.push(ManifestParseIssue {
                source_label: source.label.clone(),
                technical_name,
                path: manifest_path,
                message: format_manifest_error(&err),
            }),
        }
    }

    (found, None, parse_errors)
}

fn format_manifest_error(err: &ManifestError) -> String {
    err.to_string()
}

/// Scan every `source` (expected pre-sorted by ascending `rank`, i.e. load
/// order) for a server and resolve shadowing + dependency order across all of
/// them at once.
pub fn scan(server_id: Uuid, sources: &[AddonsSource]) -> ModuleScan {
    let mut all_listings: Vec<ModuleListing> = Vec::new();
    let mut source_errors = Vec::new();
    let mut parse_errors = Vec::new();

    for source in sources {
        let (found, source_error, mut issues) = scan_source_dir(source);
        source_errors.extend(source_error);
        parse_errors.append(&mut issues);

        for raw in found {
            all_listings.push(ModuleListing {
                technical_name: raw.technical_name,
                name: raw.manifest.name,
                version: raw.manifest.version,
                category: raw.manifest.category,
                application: raw.manifest.application,
                installable: raw.manifest.installable,
                auto_install_enabled: raw.manifest.auto_install_enabled,
                depends: raw.manifest.depends,
                source_id: source.id,
                source_label: source.label.clone(),
                source_kind: source.kind,
                rank: source.rank,
                shadowed: false, // resolved below
                path: raw.path,
            });
        }
    }

    // Winner = lowest rank per technical_name (rank 0 loads first and wins,
    // matching real addons_path resolution order).
    let mut best_rank_by_name: HashMap<String, u32> = HashMap::new();
    for listing in &all_listings {
        best_rank_by_name
            .entry(listing.technical_name.clone())
            .and_modify(|r| *r = (*r).min(listing.rank))
            .or_insert(listing.rank);
    }

    for listing in &mut all_listings {
        let best = best_rank_by_name[&listing.technical_name];
        listing.shadowed = listing.rank != best;
    }

    all_listings.sort_by(|a, b| a.technical_name.cmp(&b.technical_name).then(a.rank.cmp(&b.rank)));

    // Collisions: any technical_name that appeared from more than one source.
    let mut by_name: HashMap<&str, Vec<&ModuleListing>> = HashMap::new();
    for listing in &all_listings {
        by_name.entry(listing.technical_name.as_str()).or_default().push(listing);
    }
    let mut collisions: Vec<Collision> = by_name
        .into_iter()
        .filter(|(_, listings)| listings.len() > 1)
        .map(|(name, listings)| {
            let winner = listings.iter().find(|l| !l.shadowed).expect("exactly one winner per collided name");
            let shadowed = listings
                .iter()
                .filter(|l| l.shadowed)
                .map(|l| ShadowedCopy { source_label: l.source_label.clone(), source_kind: l.source_kind, rank: l.rank })
                .collect();
            Collision { technical_name: name.to_string(), winner_source_label: winner.source_label.clone(), winner_rank: winner.rank, shadowed }
        })
        .collect();
    collisions.sort_by(|a, b| a.technical_name.cmp(&b.technical_name));

    let winners: Vec<&ModuleListing> = all_listings.iter().filter(|l| !l.shadowed).collect();
    let winner_names: HashSet<&str> = winners.iter().map(|l| l.technical_name.as_str()).collect();

    let mut unresolved_depends = Vec::new();
    for winner in &winners {
        for dep in &winner.depends {
            if !winner_names.contains(dep.as_str()) {
                unresolved_depends.push(UnresolvedDependency { technical_name: winner.technical_name.clone(), missing_dependency: dep.clone() });
            }
        }
    }

    let (install_order, cycle) = match topological_order(&winners) {
        Ok(order) => (Some(order), None),
        Err(stuck) => (None, Some(stuck)),
    };

    ModuleScan {
        server_id,
        scanned_at: Utc::now(),
        modules: all_listings,
        collisions,
        unresolved_depends,
        install_order,
        cycle,
        source_errors,
        parse_errors,
    }
}

/// Kahn's algorithm over the winning modules only, counting an edge only when
/// the dependency is itself among the winners — an unresolved dependency
/// (reported separately) is treated as external/already-satisfied rather than
/// blocking the sort, since most real addons-path scans won't include a full
/// core Odoo checkout.
fn topological_order(winners: &[&ModuleListing]) -> Result<Vec<String>, Vec<String>> {
    let names: HashSet<&str> = winners.iter().map(|m| m.technical_name.as_str()).collect();

    let mut in_degree: HashMap<&str, usize> = names.iter().map(|&n| (n, 0)).collect();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for m in winners {
        for dep in &m.depends {
            if names.contains(dep.as_str()) {
                *in_degree.get_mut(m.technical_name.as_str()).unwrap() += 1;
                dependents.entry(dep.as_str()).or_default().push(m.technical_name.as_str());
            }
        }
    }

    let mut queue: VecDeque<&str> = in_degree.iter().filter(|(_, &d)| d == 0).map(|(&n, _)| n).collect();
    // Deterministic output regardless of HashMap iteration order.
    let mut queue_sorted: Vec<&str> = queue.drain(..).collect();
    queue_sorted.sort_unstable();
    let mut queue: VecDeque<&str> = queue_sorted.into();

    let mut order = Vec::new();
    while let Some(n) = queue.pop_front() {
        order.push(n.to_string());
        if let Some(deps) = dependents.get(n) {
            let mut newly_ready = Vec::new();
            for &d in deps {
                let e = in_degree.get_mut(d).unwrap();
                *e -= 1;
                if *e == 0 {
                    newly_ready.push(d);
                }
            }
            newly_ready.sort_unstable();
            for d in newly_ready {
                queue.push_back(d);
            }
        }
    }

    if order.len() == names.len() {
        Ok(order)
    } else {
        let done: HashSet<&str> = order.iter().map(|s| s.as_str()).collect();
        let mut stuck: Vec<String> = names.into_iter().filter(|n| !done.contains(n)).map(str::to_string).collect();
        stuck.sort();
        Err(stuck)
    }
}

#[cfg(test)]
mod tests {
    // --- discovering addons folders ---------------------------------------

    fn module_at(dir: &std::path::Path, name: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("__manifest__.py"), "{'name': 'x'}").unwrap();
    }

    /// A real Odoo checkout keeps modules in two places, and the one
    /// holding `base` is the one people forget. Registering only `addons/`
    /// is why `base` used to report as "not on disk".
    #[test]
    fn an_odoo_checkout_yields_both_of_its_addons_folders() {
        let dir = tempfile::TempDir::new().unwrap();
        let checkout = dir.path().join("odoo-real");
        std::fs::create_dir_all(checkout.join("odoo/addons")).unwrap();
        std::fs::write(checkout.join("odoo-bin"), "#!/usr/bin/env python").unwrap();
        module_at(&checkout.join("addons"), "account");
        module_at(&checkout.join("odoo/addons"), "base");

        let found = super::discover_addons_roots(&checkout, 4);
        let paths: Vec<_> = found.iter().map(|r| r.path.clone()).collect();
        assert!(paths.contains(&checkout.join("addons")), "{paths:?}");
        assert!(paths.contains(&checkout.join("odoo/addons")), "the folder holding `base` is the one that matters: {paths:?}");
        assert!(found.iter().all(|r| r.kind == SourceKind::Core), "both belong to Odoo itself");
        assert!(found.iter().all(|r| r.why.contains("Odoo checkout")), "and it says why");
    }

    /// The Doodba priority model, generated: your code first, then OCA,
    /// then Odoo's own.
    #[test]
    fn a_workspace_is_proposed_in_load_order_with_your_own_code_first() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        module_at(&root.join("client-work/addons"), "acme_sales");

        std::fs::create_dir_all(root.join("odoo/odoo/addons")).unwrap();
        std::fs::write(root.join("odoo/odoo-bin"), "#!/usr/bin/env python").unwrap();
        module_at(&root.join("odoo/addons"), "account");

        module_at(&root.join("OCA/partner-contact"), "partner_firstname");

        let found = super::discover_addons_roots(root, 4);
        let kinds: Vec<_> = found.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![SourceKind::Private, SourceKind::Oca, SourceKind::Core],
            "private > OCA > core is the whole point: {:?}",
            found.iter().map(|r| (&r.label, r.kind)).collect::<Vec<_>>()
        );
    }

    /// OCA repositories carry a `setup/` directory with one entry per
    /// module. That's a checkable signal, not a guess about the name.
    #[test]
    fn an_oca_repo_is_recognised_by_its_own_layout_not_only_its_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("some-repo");
        module_at(&repo, "partner_firstname");
        std::fs::create_dir_all(repo.join("setup/partner_firstname")).unwrap();

        let found = super::discover_addons_roots(dir.path(), 3);
        assert_eq!(found[0].kind, SourceKind::Oca);
        assert!(found[0].why.contains("setup/"));
    }

    /// Anything unrecognised is *yours*. Misfiling your own module as OCA
    /// would quietly change which copy wins a shadowing collision, so the
    /// safe default is the one that loads first.
    #[test]
    fn anything_unrecognised_is_treated_as_your_own_code() {
        let dir = tempfile::TempDir::new().unwrap();
        module_at(&dir.path().join("random_folder"), "my_module");
        let found = super::discover_addons_roots(dir.path(), 3);
        assert_eq!(found[0].kind, SourceKind::Private);
    }

    /// A folder with no modules directly in it isn't an addons path entry,
    /// whatever it's called — that's Odoo's own rule.
    #[test]
    fn a_folder_holding_no_modules_is_not_offered() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("addons")).unwrap();
        std::fs::write(dir.path().join("addons/README.md"), "empty").unwrap();
        assert!(super::discover_addons_roots(dir.path(), 3).is_empty());
    }

    /// Walking a `node_modules` or a virtualenv is thousands of folders of
    /// pure waste, and `.git` holds no modules at all.
    #[test]
    fn noisy_folders_are_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        module_at(&dir.path().join("node_modules/something"), "not_a_module");
        module_at(&dir.path().join(".git/weird"), "not_a_module");
        module_at(&dir.path().join("real"), "acme_sales");
        let found = super::discover_addons_roots(dir.path(), 4);
        assert_eq!(found.len(), 1, "{:?}", found.iter().map(|r| &r.path).collect::<Vec<_>>());
        assert!(found[0].path.ends_with("real"));
    }

    /// The label should be what a person calls the folder — the repository
    /// name, when it's in one.
    #[test]
    fn a_folder_in_a_repository_is_labelled_by_that_repository() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("acme-modules");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        module_at(&repo.join("addons"), "acme_sales");
        let found = super::discover_addons_roots(dir.path(), 4);
        assert_eq!(found[0].label, "acme-modules/addons");
    }

    /// Against the real Odoo checkout, not a fixture — the shapes a
    /// hand-built fixture gets right are the ones you thought of.
    #[test]
    fn a_real_odoo_checkout_is_read_correctly() {
        let Some(checkout) = std::env::var_os("ORCHESTRATOR_TEST_ODOO_CHECKOUT").map(PathBuf::from) else {
            eprintln!("skipping: set ORCHESTRATOR_TEST_ODOO_CHECKOUT to a real Odoo checkout");
            return;
        };
        let found = super::discover_addons_roots(&checkout, 4);
        let paths: Vec<String> = found.iter().map(|r| r.path.display().to_string()).collect();

        assert!(
            found.iter().any(|r| r.path.ends_with("odoo/addons") && r.module_count > 0),
            "the folder holding `base` must be found: {paths:?}",
        );
        assert!(found.iter().any(|r| r.path.ends_with("addons") && r.module_count > 100), "{paths:?}");
        assert!(found.iter().all(|r| r.kind == SourceKind::Core), "everything in an Odoo checkout is Odoo's own");

        let base_root = found.iter().find(|r| r.path.ends_with("odoo/addons")).unwrap();
        assert!(base_root.sample.contains(&"base".to_string()), "sample: {:?}", base_root.sample);
    }

    // --- content hashing -------------------------------------------------

    /// Content, not timestamps. `git checkout` rewrites the mtime of every
    /// file in a repo; if that counted as a change, switching branches
    /// would trigger an update of forty modules that are byte-identical.
    #[test]
    fn a_modules_hash_follows_its_contents_and_ignores_its_timestamps() {
        let dir = tempfile::TempDir::new().unwrap();
        let module = dir.path().join("acme_sales");
        std::fs::create_dir_all(module.join("views")).unwrap();
        std::fs::write(module.join("__manifest__.py"), "{'name': 'Acme'}").unwrap();
        std::fs::write(module.join("views/order.xml"), "<odoo/>").unwrap();

        let before = super::content_hash(&module).unwrap();

        // Rewriting a file with the same bytes must not change the hash.
        std::fs::write(module.join("views/order.xml"), "<odoo/>").unwrap();
        assert_eq!(super::content_hash(&module).unwrap(), before, "identical bytes are not a change");

        // Changing one byte must.
        std::fs::write(module.join("views/order.xml"), "<odoo> </odoo>").unwrap();
        assert_ne!(super::content_hash(&module).unwrap(), before);
    }

    /// A rename changes what Odoo loads, so it has to change the hash even
    /// when every byte of content is the same.
    #[test]
    fn renaming_a_file_counts_as_a_change() {
        let dir = tempfile::TempDir::new().unwrap();
        let module = dir.path().join("acme_sales");
        std::fs::create_dir_all(module.join("data")).unwrap();
        std::fs::write(module.join("data/a.xml"), "<odoo/>").unwrap();
        let before = super::content_hash(&module).unwrap();
        std::fs::rename(module.join("data/a.xml"), module.join("data/b.xml")).unwrap();
        assert_ne!(super::content_hash(&module).unwrap(), before);
    }

    /// Build output isn't a change: `.pyc` files are rewritten whenever
    /// Python feels like it, and hashing them would mean every module
    /// looked stale after any run.
    #[test]
    fn compiled_python_and_dot_directories_do_not_count() {
        let dir = tempfile::TempDir::new().unwrap();
        let module = dir.path().join("acme_sales");
        std::fs::create_dir_all(module.join("__pycache__")).unwrap();
        std::fs::create_dir_all(module.join(".git")).unwrap();
        std::fs::write(module.join("__manifest__.py"), "{'name': 'Acme'}").unwrap();
        let before = super::content_hash(&module).unwrap();

        std::fs::write(module.join("__pycache__/models.cpython-311.pyc"), "compiled").unwrap();
        std::fs::write(module.join("models.pyc"), "compiled").unwrap();
        std::fs::write(module.join(".git/HEAD"), "ref: refs/heads/other").unwrap();
        assert_eq!(super::content_hash(&module).unwrap(), before, "none of that is a change to the module");
    }

    #[test]
    fn a_folder_with_nothing_in_it_has_no_hash() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(super::content_hash(&dir.path().join("nothing_here")).is_none());
    }

    use super::*;
    use tempfile::TempDir;

    fn write_module(root: &Path, technical_name: &str, manifest_body: &str) {
        let dir = root.join(technical_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("__manifest__.py"), manifest_body).unwrap();
    }

    fn source(id: Uuid, label: &str, dir: &TempDir, kind: SourceKind, rank: u32) -> AddonsSource {
        AddonsSource {
            id,
            server_id: Uuid::new_v4(),
            label: label.to_string(),
            path_or_url: dir.path().to_string_lossy().to_string(),
            kind,
            rank,
        }
    }

    #[test]
    fn scans_a_single_source_with_no_collisions() {
        let dir = TempDir::new().unwrap();
        write_module(dir.path(), "sale_custom", "{'name': 'Sale Custom', 'depends': ['base']}");
        write_module(dir.path(), "base", "{'name': 'Base'}");

        let src = source(Uuid::new_v4(), "this repo", &dir, SourceKind::Private, 0);
        let scan = scan(Uuid::new_v4(), &[src]);

        assert_eq!(scan.modules.len(), 2);
        assert!(scan.modules.iter().all(|m| !m.shadowed));
        assert!(scan.collisions.is_empty());
        assert!(scan.unresolved_depends.is_empty(), "base is present, so sale_custom's dep resolves");
        assert_eq!(scan.install_order.as_deref(), Some(&["base".to_string(), "sale_custom".to_string()][..]));
    }

    #[test]
    fn lower_rank_source_wins_a_shadowing_collision() {
        let private_dir = TempDir::new().unwrap();
        write_module(private_dir.path(), "sale_custom", "{'name': 'My Sale Custom'}");

        let oca_dir = TempDir::new().unwrap();
        write_module(oca_dir.path(), "sale_custom", "{'name': 'OCA Sale Custom'}");

        let private_src = source(Uuid::new_v4(), "this repo", &private_dir, SourceKind::Private, 0);
        let oca_src = source(Uuid::new_v4(), "OCA mirror", &oca_dir, SourceKind::Oca, 1);

        let scan = scan(Uuid::new_v4(), &[private_src, oca_src]);

        assert_eq!(scan.collisions.len(), 1);
        let collision = &scan.collisions[0];
        assert_eq!(collision.technical_name, "sale_custom");
        assert_eq!(collision.winner_source_label, "this repo");
        assert_eq!(collision.shadowed.len(), 1);
        assert_eq!(collision.shadowed[0].source_label, "OCA mirror");

        let winner = scan.modules.iter().find(|m| !m.shadowed).unwrap();
        assert_eq!(winner.name, "My Sale Custom");
        let loser = scan.modules.iter().find(|m| m.shadowed).unwrap();
        assert_eq!(loser.name, "OCA Sale Custom");
    }

    #[test]
    fn reports_unresolved_dependencies_without_failing_the_scan() {
        let dir = TempDir::new().unwrap();
        write_module(dir.path(), "base", "{'name': 'Base'}");
        write_module(dir.path(), "sale_custom", "{'name': 'x', 'depends': ['base', 'some_missing_module']}");
        let src = source(Uuid::new_v4(), "this repo", &dir, SourceKind::Private, 0);

        let scan = scan(Uuid::new_v4(), &[src]);
        assert_eq!(scan.unresolved_depends.len(), 1);
        assert_eq!(scan.unresolved_depends[0].missing_dependency, "some_missing_module");
        // A missing dep doesn't block the topological sort — it's just not an edge.
        assert!(scan.install_order.is_some());
    }

    #[test]
    fn detects_a_genuine_dependency_cycle() {
        let dir = TempDir::new().unwrap();
        write_module(dir.path(), "a", "{'name': 'a', 'depends': ['b']}");
        write_module(dir.path(), "b", "{'name': 'b', 'depends': ['a']}");
        let src = source(Uuid::new_v4(), "this repo", &dir, SourceKind::Private, 0);

        let scan = scan(Uuid::new_v4(), &[src]);
        assert!(scan.install_order.is_none());
        let cycle = scan.cycle.expect("a<->b is a real cycle");
        assert_eq!(cycle, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn missing_source_directory_is_reported_not_fatal() {
        let src = AddonsSource {
            id: Uuid::new_v4(),
            server_id: Uuid::new_v4(),
            label: "gone".to_string(),
            path_or_url: "/definitely/does/not/exist".to_string(),
            kind: SourceKind::Private,
            rank: 0,
        };
        let scan = scan(Uuid::new_v4(), &[src]);
        assert!(scan.modules.is_empty());
        assert_eq!(scan.source_errors.len(), 1);
        assert_eq!(scan.source_errors[0].source_label, "gone");
    }

    #[test]
    fn a_broken_manifest_is_reported_and_skipped_not_fatal() {
        let dir = TempDir::new().unwrap();
        write_module(dir.path(), "good", "{'name': 'Good'}");
        write_module(dir.path(), "broken", "{ this is not valid python");
        let src = source(Uuid::new_v4(), "this repo", &dir, SourceKind::Private, 0);

        let scan = scan(Uuid::new_v4(), &[src]);
        assert_eq!(scan.modules.len(), 1, "the broken module is excluded, not the whole scan");
        assert_eq!(scan.modules[0].technical_name, "good");
        assert_eq!(scan.parse_errors.len(), 1);
        assert_eq!(scan.parse_errors[0].technical_name, "broken");
    }

    #[test]
    fn three_way_collision_keeps_only_the_lowest_rank_as_winner() {
        let d0 = TempDir::new().unwrap();
        write_module(d0.path(), "m", "{'name': 'rank0'}");
        let d1 = TempDir::new().unwrap();
        write_module(d1.path(), "m", "{'name': 'rank1'}");
        let d2 = TempDir::new().unwrap();
        write_module(d2.path(), "m", "{'name': 'rank2'}");

        let sources = vec![
            source(Uuid::new_v4(), "s0", &d0, SourceKind::Private, 0),
            source(Uuid::new_v4(), "s1", &d1, SourceKind::Oca, 1),
            source(Uuid::new_v4(), "s2", &d2, SourceKind::Core, 2),
        ];
        let scan = scan(Uuid::new_v4(), &sources);

        assert_eq!(scan.collisions.len(), 1);
        assert_eq!(scan.collisions[0].shadowed.len(), 2);
        let winner = scan.modules.iter().find(|m| !m.shadowed).unwrap();
        assert_eq!(winner.name, "rank0");
    }
}
