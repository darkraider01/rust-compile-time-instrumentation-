use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SESSION_ENV: &str = "CARGO_INSTRUMENT_SESSION";

/// Reason why compile-time instrumentation was skipped for a compilation unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipCause {
    /// Host-only unit reached via build-dependency or proc-macro target.
    HostOnlyPackage,
    /// Crate is part of the telemetry runtime or instrumentation tool itself.
    TelemetryOrToolCrate,
    /// Uninstrumented registry dependency (opt-in via CARGO_INSTRUMENT_REGISTRY not set).
    RegistryDependencyUnconfigured,
    /// Missing otel-shim link provider in the build graph (Tier 2 trampoline link safety).
    MissingOtelShimProvider,
    /// Crate has `#![no_std]` attribute.
    NoStd,
    /// Crate has `#![forbid(unsafe_code)]`.
    ForbiddenUnsafe,
    /// Symbol collision with otel-shim ABI.
    SymbolCollision,
    /// Preflight check failed for application crate declaring otel-shim.
    PreflightFailed(String),
}

/// Global build session policy constructed once per Cargo build via `cargo metadata`.
///
/// Cargo is responsible for scheduling, ordering, parallelism, and unit resolution.
/// `SessionPlan` provides identity and topology knowledge to the per-unit wrapper:
/// 1. Whether any target root links `otel-shim` (preventing unresolved C-ABI trampolines).
/// 2. Which packages are compiled exclusively for the host (preventing proc-macro dep instrumentation).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionPlan {
    /// Whether any target root in the build graph has otel-shim reachable.
    pub has_otel_shim_provider: bool,
    /// Set of package names that are compiled exclusively for the host
    /// (reached solely via build-dependencies or from proc-macro packages).
    pub host_only_packages: HashSet<String>,
    /// Set of package manifest directories compiled exclusively for the host.
    #[serde(default)]
    pub host_only_manifest_dirs: HashSet<PathBuf>,
    /// Set of package names that have proc-macro targets.
    #[serde(default)]
    pub proc_macro_packages: HashSet<String>,
}

impl SessionPlan {
    /// Returns true if the package is compiled exclusively for the host
    /// (e.g. a dependency of a proc-macro or build script, not linked into target artifacts).
    pub fn is_host_only(&self, package_name: &str) -> bool {
        self.host_only_packages.contains(package_name)
    }

    /// Disambiguated check: returns true if the package is host-only by name or by source path.
    pub fn is_host_only_unit(&self, package_name: &str, source_file: Option<&Path>) -> bool {
        if self.host_only_packages.contains(package_name) {
            return true;
        }
        if let Some(src) = source_file {
            if self
                .host_only_manifest_dirs
                .iter()
                .any(|dir| src.starts_with(dir))
            {
                return true;
            }
        }
        false
    }

    /// Returns true if `otel-shim` is reachable in the target dependency graph.
    pub fn has_otel_shim_provider(&self) -> bool {
        self.has_otel_shim_provider
    }

    /// Load existing session plan from file, or query `cargo metadata` once and cache it.
    /// Find the most likely root workspace or application manifest directory.
    pub fn find_best_manifest_dir(current_dir: &Path, out_dir: Option<&Path>) -> PathBuf {
        let mut candidates: Vec<(PathBuf, i32)> = Vec::new();

        let mut add_candidate = |dir: &Path, base_score: i32| {
            let manifest = dir.join("Cargo.toml");
            if manifest.is_file() {
                let mut score = base_score;
                if dir.join("src").join("main.rs").is_file() || dir.join("src").join("bin").is_dir()
                {
                    score += 100;
                }
                if let Ok(content) = fs::read_to_string(&manifest) {
                    if content.contains("[workspace]") && content.contains("members") {
                        score += 80;
                    }
                    if content.contains("[[bin]]") {
                        score += 60;
                    }
                    if content.contains("otel-shim") || content.contains("otel_shim") {
                        score += 40;
                    }
                    if content.contains("[dependencies]") {
                        score += 20;
                    }
                }
                if !candidates.iter().any(|(c, _)| c == dir) {
                    candidates.push((dir.to_path_buf(), score));
                }
            }
        };

        // 1. Check current_dir and its ancestors
        let mut curr = Some(current_dir);
        let mut depth = 0;
        while let Some(dir) = curr {
            if depth > 6 {
                break;
            }
            add_candidate(dir, if depth == 0 { 10 } else { 0 });
            curr = dir.parent();
            depth += 1;
        }

        // 2. Check out_dir ancestors bounded to the project root containing 'target'
        if let Some(out) = out_dir {
            let mut root_boundary = None;
            for ancestor in out.ancestors() {
                if ancestor.file_name().and_then(|n| n.to_str()) == Some("target") {
                    root_boundary = ancestor.parent();
                    break;
                }
            }

            if let Some(root) = root_boundary {
                add_candidate(root, 0);
                if let Ok(entries) = fs::read_dir(root) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                            if file_name != "target" && !file_name.starts_with('.') {
                                add_candidate(&path, 0);
                            }
                        }
                    }
                }
            } else {
                // If no 'target' in path, check ancestors up to depth 5, stopping at system temp
                let temp = std::env::temp_dir();
                for ancestor in out.ancestors().take(5) {
                    if ancestor == temp {
                        break;
                    }
                    add_candidate(ancestor, 0);
                }
            }
        }

        candidates.sort_by_key(|b| std::cmp::Reverse(b.1));

        candidates
            .first()
            .map(|(p, _)| p.clone())
            .unwrap_or_else(|| current_dir.to_path_buf())
    }

    /// Load existing session plan from file, or query `cargo metadata` once and cache it.
    pub fn load_or_create(current_dir: &Path, out_dir: Option<&Path>) -> Self {
        // 1. Check CARGO_INSTRUMENT_SESSION env var
        if let Ok(path_str) = std::env::var(SESSION_ENV) {
            let path = PathBuf::from(path_str);
            if path.exists() {
                if let Ok(plan) = Self::load_from_file(&path) {
                    return plan;
                }
            }
            let best_dir = Self::find_best_manifest_dir(current_dir, out_dir);
            if let Ok(plan) = Self::build_from_metadata(&best_dir) {
                let _ = plan.save_to_file(&path);
                return plan;
            }
        }

        // 2. Check cache file in out_dir
        if let Some(out) = out_dir {
            let session_file = out.join("cargo_instrument_session.json");
            if session_file.exists() {
                if let Ok(meta) = fs::metadata(&session_file) {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(elapsed) = modified.elapsed() {
                            if elapsed.as_secs() < 1800 {
                                if let Ok(plan) = Self::load_from_file(&session_file) {
                                    return plan;
                                }
                            }
                        }
                    }
                }
            }

            let best_dir = Self::find_best_manifest_dir(current_dir, out_dir);
            if let Ok(plan) = Self::build_from_metadata(&best_dir) {
                let _ = plan.save_to_file(&session_file);
                return plan;
            }
        }

        // Fallback: build without caching, or default (D5)
        let best_dir = Self::find_best_manifest_dir(current_dir, out_dir);
        match Self::build_from_metadata(&best_dir) {
            Ok(plan) => plan,
            Err(e) => {
                eprintln!(
                    "warning: cargo-instrument: failed to compute session plan from metadata ({e}). \
                     Defaulting to no-shim provider per S11 fail-open."
                );
                Self::default()
            }
        }
    }

    /// Query `cargo metadata` and build a `SessionPlan`.
    pub fn build_from_metadata(manifest_dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut cmd = Command::new("cargo");
        cmd.arg("metadata")
            .arg("--format-version")
            .arg("1")
            .current_dir(manifest_dir);

        let output = cmd.output()?;
        if !output.status.success() {
            return Err(format!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        Self::from_metadata_json(&json)
    }

    /// Parse metadata JSON and compute target vs host package sets.
    pub fn from_metadata_json(
        json: &serde_json::Value,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let packages = json["packages"]
            .as_array()
            .ok_or("missing packages in metadata")?;

        let mut pkg_id_to_name: HashMap<String, String> = HashMap::new();
        let mut proc_macro_packages: HashSet<String> = HashSet::new();
        let mut proc_macro_ids: HashSet<String> = HashSet::new();

        for pkg in packages {
            if let (Some(id), Some(name)) = (pkg["id"].as_str(), pkg["name"].as_str()) {
                pkg_id_to_name.insert(id.to_string(), name.to_string());
                if let Some(targets) = pkg["targets"].as_array() {
                    let is_pm = targets.iter().any(|t| {
                        t["kind"]
                            .as_array()
                            .map(|k| k.iter().any(|v| v == "proc-macro"))
                            .unwrap_or(false)
                    });
                    if is_pm {
                        proc_macro_packages.insert(name.to_string());
                        proc_macro_packages.insert(name.replace('-', "_"));
                        proc_macro_ids.insert(id.to_string());
                    }
                }
            }
        }

        // Identify workspace members
        let workspace_members: HashSet<String> = json["workspace_members"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let nodes = match json["resolve"]["nodes"].as_array() {
            Some(n) => n,
            None => {
                // If no resolve, check packages directly
                let has_shim = packages.iter().any(|p| {
                    p["name"]
                        .as_str()
                        .map(|n| n == "otel-shim" || n == "otel_shim")
                        .unwrap_or(false)
                });
                return Ok(Self {
                    has_otel_shim_provider: has_shim,
                    host_only_packages: HashSet::new(),
                    host_only_manifest_dirs: HashSet::new(),
                    proc_macro_packages,
                });
            }
        };

        // Adjacency map: node_id -> Vec<(dep_pkg_id, Vec<Option<kind>>)>
        type DepEdge = (String, Vec<Option<String>>);
        let mut node_deps: HashMap<String, Vec<DepEdge>> = HashMap::new();
        for node in nodes {
            if let Some(id) = node["id"].as_str() {
                let mut deps_list = Vec::new();
                if let Some(deps) = node["deps"].as_array() {
                    for dep in deps {
                        if let Some(dep_pkg) = dep["pkg"].as_str() {
                            let mut kinds = Vec::new();
                            if let Some(dep_kinds) = dep["dep_kinds"].as_array() {
                                for dk in dep_kinds {
                                    let k = dk["kind"].as_str().map(String::from);
                                    kinds.push(k);
                                }
                            }
                            if kinds.is_empty() {
                                kinds.push(None); // Default is normal target dep
                            }
                            deps_list.push((dep_pkg.to_string(), kinds));
                        }
                    }
                }
                node_deps.insert(id.to_string(), deps_list);
            }
        }

        // Determine target roots: workspace members that are not proc-macros
        let mut target_roots: Vec<String> = workspace_members
            .iter()
            .filter(|id| !proc_macro_ids.contains(*id))
            .cloned()
            .collect();

        if target_roots.is_empty() {
            // If all members are proc-macros or empty, use any non-proc-macro package
            for id in pkg_id_to_name.keys() {
                if !proc_macro_ids.contains(id) {
                    target_roots.push(id.clone());
                }
            }
        }

        // BFS to find all packages reachable from target roots via normal or dev dependencies
        let mut target_reachable_ids: HashSet<String> = HashSet::new();
        let mut queue = std::collections::VecDeque::new();

        for root in target_roots {
            if target_reachable_ids.insert(root.clone()) {
                queue.push_back(root);
            }
        }

        while let Some(current) = queue.pop_front() {
            if let Some(deps) = node_deps.get(&current) {
                for (dep_id, kinds) in deps {
                    // An edge is a target edge if it is a normal dep (kind is None) or dev dep (kind is Some("dev"))
                    let is_target_edge = kinds
                        .iter()
                        .any(|k| k.is_none() || k.as_deref() == Some("dev"));
                    if is_target_edge
                        && !proc_macro_ids.contains(dep_id)
                        && target_reachable_ids.insert(dep_id.clone())
                    {
                        queue.push_back(dep_id.clone());
                    }
                }
            }
        }

        // Target-reachable package names
        let target_reachable_names: HashSet<String> = target_reachable_ids
            .iter()
            .filter_map(|id| pkg_id_to_name.get(id).cloned())
            .collect();

        // Any package in the build graph whose name is NEVER reachable via target roots is host-only.
        // D2: Store both original and underscore-normalized names so rustc `--crate-name` matches.
        let mut host_only_packages: HashSet<String> = HashSet::new();
        for name in pkg_id_to_name.values() {
            if !target_reachable_names.contains(name) {
                host_only_packages.insert(name.clone());
                host_only_packages.insert(name.replace('-', "_"));
            }
        }

        // Exact manifest directories of packages that are not reachable from target roots
        let mut host_only_manifest_dirs: HashSet<PathBuf> = HashSet::new();
        for pkg in packages {
            if let (Some(id), Some(manifest)) = (pkg["id"].as_str(), pkg["manifest_path"].as_str())
            {
                if !target_reachable_ids.contains(id) {
                    if let Some(parent) = Path::new(manifest).parent() {
                        host_only_manifest_dirs.insert(parent.to_path_buf());
                    }
                }
            }
        }

        // Check if otel-shim is reachable from target packages
        let has_otel_shim_provider = target_reachable_ids.iter().any(|id| {
            pkg_id_to_name
                .get(id)
                .map(|name| name == "otel-shim" || name == "otel_shim")
                .unwrap_or(false)
        });

        Ok(Self {
            has_otel_shim_provider,
            host_only_packages,
            host_only_manifest_dirs,
            proc_macro_packages,
        })
    }

    pub fn save_to_file(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(self)?;
        static SESSION_TEMP_COUNTER: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let counter = SESSION_TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp_path = path.with_extension(format!("tmp.{}.{}", std::process::id(), counter));
        fs::write(&temp_path, &data)?;
        if let Err(e) = fs::rename(&temp_path, path) {
            let _ = fs::remove_file(&temp_path);
            fs::write(path, &data).map_err(|write_err| {
                format!("rename failed ({e}); direct write failed ({write_err})")
            })?;
        }
        Ok(())
    }

    pub fn load_from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read(path)?;
        let plan: Self = serde_json::from_slice(&content)?;
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_d5_default_plan_disables_otel_shim_provider() {
        let plan = SessionPlan::default();
        assert!(
            !plan.has_otel_shim_provider(),
            "Default plan must default has_otel_shim_provider to false per S11 fail-open"
        );
    }

    #[test]
    fn test_d2_host_only_hyphenated_and_underscored_matching() {
        let mut plan = SessionPlan::default();
        plan.host_only_packages.insert("pm-dep".to_string());
        plan.host_only_packages.insert("pm_dep".to_string());

        assert!(plan.is_host_only("pm-dep"));
        assert!(plan.is_host_only("pm_dep"));
        assert!(plan.is_host_only_unit("pm_dep", None));
        assert!(plan.is_host_only_unit("pm-dep", None));
    }
}
