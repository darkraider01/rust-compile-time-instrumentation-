use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SESSION_ENV: &str = "CARGO_INSTRUMENT_SESSION";

/// The portion of Cargo's compiler-artifact profile that must agree with a wrapped rustc unit.
/// A missing or mismatched profile is a fail-open condition for native R-4 injection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct R4Profile {
    pub opt_level: String,
    pub debug_assertions: bool,
    pub overflow_checks: bool,
    pub test: bool,
}

impl Default for R4Profile {
    fn default() -> Self {
        // Cargo's dev profile omits these defaults from rustc argv, so absence must not be
        // mistaken for an unknown profile. Release/custom profiles carry explicit `-C` values.
        Self {
            opt_level: "0".to_string(),
            debug_assertions: true,
            overflow_checks: true,
            test: false,
        }
    }
}

/// A native OpenTelemetry artifact reported by Cargo's `compiler-artifact` JSON message.
///
/// The path is authoritative Cargo output, never the result of a filename search by this tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct R4NativeOtelArtifact {
    pub package_id: String,
    pub package_version: String,
    pub resolved_features: Vec<String>,
    pub target: Option<String>,
    pub profile: R4Profile,
    pub rlib_path: PathBuf,
}

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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionPlan {
    /// Whether any target root in the build graph has otel-shim reachable.
    /// D5 / S11: Defaults to false to fail open if metadata evaluation fails.
    pub has_otel_shim_provider: bool,
    /// Set of package names that are reached by at least one target root
    /// that does not link otel-shim (G7).
    #[serde(default)]
    pub shim_unsafe_packages: HashSet<String>,
    /// Set of package names that are compiled exclusively for the host
    /// (reached solely via build-dependencies or from proc-macro packages).
    pub host_only_packages: HashSet<String>,
    /// Set of package manifest directories compiled exclusively for the host.
    #[serde(default)]
    pub host_only_manifest_dirs: HashSet<PathBuf>,
    /// Content hash over Cargo.lock + reachable member Cargo.toml manifests.
    #[serde(default)]
    pub fingerprint: String,
    /// List of member / local manifest files included in the fingerprint.
    #[serde(default)]
    pub manifest_paths: Vec<PathBuf>,
    /// Resolved root workspace directory.
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
    /// Cargo package id -> manifest directory, used to identify a wrapped dependency exactly.
    #[serde(default)]
    pub package_manifest_dirs: HashMap<String, PathBuf>,
    /// All non-host Cargo package IDs reachable from at least one target root.  H1 uses this
    /// authoritative set to invalidate units which may be handled by either native R-4 or the
    /// Tier-2 wrapper path after an uninstrumented pre-pass.
    #[serde(default)]
    pub target_reachable_package_ids: HashSet<String>,
    /// Dependency package id -> the sole OpenTelemetry package id shared by every target root
    /// that reaches it. Omitted when roots disagree or no target-safe choice exists.
    #[serde(default)]
    pub r4_otel_package_by_dependency: HashMap<String, String>,
    /// Exact Cargo-reported artifacts available to the R-4 resolver.
    #[serde(default)]
    pub r4_native_otel_artifacts: Vec<R4NativeOtelArtifact>,
}

impl SessionPlan {
    /// Returns true if the package is compiled exclusively for the host
    /// (e.g. a dependency of a proc-macro or build script, not linked into target artifacts).
    ///
    /// D2: normalizes both hyphenated and underscored package names.
    pub fn is_host_only(&self, package_name: &str) -> bool {
        self.host_only_packages
            .contains(&package_name.replace('-', "_"))
    }

    /// Disambiguated check: returns true if the package is host-only by name or by source path.
    ///
    /// D2: normalizes both hyphenated and underscored package names.
    pub fn is_host_only_unit(&self, package_name: &str, source_file: Option<&Path>) -> bool {
        if self
            .host_only_packages
            .contains(&package_name.replace('-', "_"))
        {
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

    /// Returns true if the package is reached by at least one target root without otel-shim (G7).
    ///
    /// D2: normalizes both hyphenated and underscored package names.
    pub fn is_shim_unsafe(&self, package_name: &str) -> bool {
        self.shim_unsafe_packages
            .contains(&package_name.replace('-', "_"))
    }

    /// Returns true if `otel-shim` is reachable in the target dependency graph.
    pub fn has_otel_shim_provider(&self) -> bool {
        self.has_otel_shim_provider
    }

    /// Adds authoritative OpenTelemetry artifacts from newline-delimited Cargo JSON output.
    ///
    /// Callers must run Cargo with `--message-format=json` for the same target/profile they
    /// intend to instrument. This routine rejects missing or ambiguous rlib output rather than
    /// inspecting a dependency directory itself.
    pub fn add_r4_artifacts_from_cargo_json(
        &mut self,
        metadata: &serde_json::Value,
        cargo_messages: &[u8],
        target: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let features_by_package = metadata["resolve"]["nodes"]
            .as_array()
            .map(|nodes| {
                nodes
                    .iter()
                    .filter_map(|node| {
                        let id = node["id"].as_str()?;
                        let mut features: Vec<String> = node["features"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|feature| feature.as_str().map(String::from))
                            .collect();
                        features.sort();
                        Some((id.to_string(), features))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let versions_by_package = metadata["packages"]
            .as_array()
            .map(|packages| {
                packages
                    .iter()
                    .filter_map(|package| {
                        Some((
                            package["id"].as_str()?.to_string(),
                            package["version"].as_str()?.to_string(),
                        ))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();

        let wanted_packages: HashSet<&str> = self
            .r4_otel_package_by_dependency
            .values()
            .map(String::as_str)
            .collect();
        if wanted_packages.is_empty() {
            return Ok(());
        }

        let mut artifacts = Vec::new();
        for line in cargo_messages.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let message: serde_json::Value = serde_json::from_slice(line)
                .map_err(|error| format!("malformed Cargo JSON artifact message: {error}"))?;
            if message["reason"].as_str() != Some("compiler-artifact") {
                continue;
            }
            let package_id = match message["package_id"].as_str() {
                Some(id) if wanted_packages.contains(id) => id,
                _ => continue,
            };
            let rlibs: Vec<PathBuf> = message["filenames"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|filename| filename.as_str())
                .map(PathBuf::from)
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "rlib")
                })
                .collect();
            if rlibs.len() != 1 {
                return Err(format!(
                    "Cargo compiler-artifact for '{package_id}' reported {} rlib files; expected exactly one",
                    rlibs.len()
                )
                .into());
            }
            let profile = R4Profile {
                opt_level: message["profile"]["opt_level"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                debug_assertions: message["profile"]["debug_assertions"]
                    .as_bool()
                    .unwrap_or(false),
                overflow_checks: message["profile"]["overflow_checks"]
                    .as_bool()
                    .unwrap_or(false),
                test: message["profile"]["test"].as_bool().unwrap_or(false),
            };
            artifacts.push(R4NativeOtelArtifact {
                package_id: package_id.to_string(),
                package_version: versions_by_package
                    .get(package_id)
                    .cloned()
                    .ok_or_else(|| format!("Cargo metadata lacks version for '{package_id}'"))?,
                resolved_features: features_by_package.get(package_id).cloned().ok_or_else(
                    || format!("Cargo metadata lacks resolved features for '{package_id}'"),
                )?,
                target: target.clone(),
                profile,
                rlib_path: rlibs.into_iter().next().unwrap(),
            });
        }

        for artifact in artifacts {
            if !artifact.rlib_path.is_file() {
                return Err(format!(
                    "Cargo-reported OpenTelemetry artifact '{}' does not exist",
                    artifact.rlib_path.display()
                )
                .into());
            }
            if self.r4_native_otel_artifacts.iter().any(|existing| {
                existing.package_id == artifact.package_id
                    && existing.target == artifact.target
                    && existing.profile == artifact.profile
            }) {
                return Err(format!(
                    "multiple Cargo artifacts match OpenTelemetry package '{}' for the same target/profile",
                    artifact.package_id
                )
                .into());
            }
            self.r4_native_otel_artifacts.push(artifact);
        }
        Ok(())
    }

    /// Resolves the exact native OpenTelemetry artifact for one wrapped dependency unit.
    ///
    /// `Ok(None)` means this package was not selected for R-4 native injection. `Err` is an
    /// ambiguity, stale path, profile mismatch, or target mismatch and must fall open to Tier-2.
    pub fn r4_native_otel_artifact_for(
        &self,
        source_file: &Path,
        rustc_args: &[String],
    ) -> Result<Option<&R4NativeOtelArtifact>, String> {
        let package_id = self
            .package_manifest_dirs
            .iter()
            .filter(|(_, manifest_dir)| source_file.starts_with(manifest_dir.as_path()))
            .max_by_key(|(_, manifest_dir)| manifest_dir.components().count())
            .map(|(package_id, _)| package_id);
        let Some(package_id) = package_id else {
            return Ok(None);
        };
        let Some(otel_package_id) = self.r4_otel_package_by_dependency.get(package_id) else {
            return Ok(None);
        };
        let target = rustc_target(rustc_args);
        let profile = rustc_profile(rustc_args);
        let matches: Vec<_> = self
            .r4_native_otel_artifacts
            .iter()
            .filter(|artifact| {
                artifact.package_id == *otel_package_id
                    && artifact.target == target
                    && artifact.profile == profile
            })
            .collect();
        match matches.as_slice() {
            [] => Err(format!(
                "no Cargo-authoritative OpenTelemetry artifact for package '{otel_package_id}', target {target:?}, profile {profile:?}; available artifacts: {:?}",
                self.r4_native_otel_artifacts,
            )),
            [artifact] if artifact.rlib_path.is_file() => Ok(Some(*artifact)),
            [artifact] => Err(format!(
                "Cargo-authoritative OpenTelemetry artifact '{}' is no longer present",
                artifact.rlib_path.display()
            )),
            _ => Err(format!(
                "multiple Cargo-authoritative OpenTelemetry artifacts match package '{otel_package_id}', target {target:?}, profile {profile:?}"
            )),
        }
    }

    /// Compute a SHA-256 fingerprint over Cargo.lock and all workspace/local Cargo.toml manifests.
    pub fn compute_fingerprint(workspace_root: &Path, manifest_paths: &[PathBuf]) -> String {
        let mut hasher = Sha256::new();

        // 1. Hash Cargo.lock in workspace root (if present)
        let lock_file = workspace_root.join("Cargo.lock");
        if let Ok(bytes) = fs::read(&lock_file) {
            hasher.update(b"lock:");
            hasher.update(&bytes);
        } else {
            hasher.update(b"lock:none");
        }

        // 2. Hash workspace_root Cargo.toml (if present)
        let root_toml = workspace_root.join("Cargo.toml");
        if let Ok(bytes) = fs::read(&root_toml) {
            hasher.update(b"root_toml:");
            hasher.update(&bytes);
        } else {
            hasher.update(b"root_toml:none");
        }

        // 3. Hash member and local dependency Cargo.toml files
        let mut sorted_paths = manifest_paths.to_vec();
        sorted_paths.sort();
        sorted_paths.dedup();

        for p in sorted_paths {
            hasher.update(p.to_string_lossy().as_bytes());
            if let Ok(bytes) = fs::read(&p) {
                hasher.update(&bytes);
            } else {
                hasher.update(b"missing");
            }
        }

        format!("{:x}", hasher.finalize())
    }

    /// Verify whether the cached session plan's fingerprint matches the current filesystem state (D1).
    pub fn is_fresh(&self, current_manifest_dir: &Path) -> bool {
        if self.fingerprint.is_empty() {
            return false;
        }

        let root = self
            .workspace_root
            .as_deref()
            .unwrap_or(current_manifest_dir);
        let current_fingerprint = Self::compute_fingerprint(root, &self.manifest_paths);
        current_fingerprint == self.fingerprint
    }

    /// Resolves the unified session cache file location from an output directory.
    ///
    /// Libraries have `--out-dir <target>/debug/deps`, while binaries have `--out-dir <target>/debug`.
    /// Stripping the trailing `deps` ensures both units share the exact same session plan file.
    pub fn get_session_file_path(out_dir: &Path) -> PathBuf {
        let base = if out_dir.file_name().and_then(|n| n.to_str()) == Some("deps") {
            out_dir.parent().unwrap_or(out_dir)
        } else {
            out_dir
        };
        base.join("cargo_instrument_session.json")
    }

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
        let best_dir = Self::find_best_manifest_dir(current_dir, out_dir);

        // 1. Check CARGO_INSTRUMENT_SESSION env var (preferred path from CLI / D3)
        if let Ok(path_str) = std::env::var(SESSION_ENV) {
            let path = PathBuf::from(path_str);
            if path.exists() {
                if let Ok(plan) = Self::load_from_file(&path) {
                    eprintln!(
                        "[DEBUG loaded plan from {}] r4_map: {:?}",
                        path.display(),
                        plan.r4_otel_package_by_dependency
                    );
                    return plan;
                }
            }
            match Self::build_from_metadata(&best_dir) {
                Ok(plan) => {
                    let _ = plan.save_to_file(&path);
                    return plan;
                }
                Err(e) => {
                    eprintln!(
                        "warning: cargo-instrument: failed to compute session plan from metadata ({e}). \
                         Defaulting to no-shim provider per S11 fail-open."
                    );
                    return Self::default();
                }
            }
        }

        // 2. Check cache file in out_dir (fallback for raw RUSTC_WRAPPER invocations)
        if let Some(out) = out_dir {
            let session_file = Self::get_session_file_path(out);
            if session_file.exists() {
                if let Ok(plan) = Self::load_from_file(&session_file) {
                    // D1: Verify fingerprint freshness instead of arbitrary time elapsed window
                    if plan.is_fresh(&best_dir) {
                        return plan;
                    }
                }
            }

            match Self::build_from_metadata(&best_dir) {
                Ok(plan) => {
                    let _ = plan.save_to_file(&session_file);
                    return plan;
                }
                Err(e) => {
                    eprintln!(
                        "warning: cargo-instrument: failed to compute session plan from metadata ({e}). \
                         Defaulting to no-shim provider per S11 fail-open."
                    );
                    return Self::default();
                }
            }
        }

        // Fallback: build without caching, or default (D5)
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
        Self::from_metadata_json_scoped(json, None)
    }

    /// Parse metadata JSON and compute target vs host package sets, optionally scoping
    /// target roots to explicit workspace package IDs selected by the invocation.
    pub fn from_metadata_json_scoped(
        json: &serde_json::Value,
        selected_package_ids: Option<&HashSet<String>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let packages = json["packages"]
            .as_array()
            .ok_or("missing packages in metadata")?;

        let mut pkg_id_to_name: HashMap<String, String> = HashMap::new();
        let mut package_manifest_dirs: HashMap<String, PathBuf> = HashMap::new();
        let mut proc_macro_ids: HashSet<String> = HashSet::new();
        let mut bin_or_cdylib_ids: HashSet<String> = HashSet::new();

        for pkg in packages {
            if let (Some(id), Some(name)) = (pkg["id"].as_str(), pkg["name"].as_str()) {
                pkg_id_to_name.insert(id.to_string(), name.to_string());
                if let Some(manifest_path) = pkg["manifest_path"].as_str() {
                    if let Some(manifest_dir) = Path::new(manifest_path).parent() {
                        package_manifest_dirs.insert(id.to_string(), manifest_dir.to_path_buf());
                    }
                }
                if let Some(targets) = pkg["targets"].as_array() {
                    let is_pm = targets.iter().any(|t| {
                        t["kind"]
                            .as_array()
                            .map(|k| k.iter().any(|v| v == "proc-macro"))
                            .unwrap_or(false)
                    });
                    if is_pm {
                        proc_macro_ids.insert(id.to_string());
                    }
                    let is_executable = targets.iter().any(|t| {
                        t["kind"]
                            .as_array()
                            .map(|k| k.iter().any(|v| v == "bin" || v == "cdylib"))
                            .unwrap_or(false)
                    });
                    if is_executable {
                        bin_or_cdylib_ids.insert(id.to_string());
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

        let workspace_root = json["workspace_root"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        // Collect all local manifest paths for fingerprinting (D1)
        let mut manifest_paths: Vec<PathBuf> = Vec::new();
        for pkg in packages {
            if pkg["source"].is_null() {
                if let Some(mp) = pkg["manifest_path"].as_str() {
                    manifest_paths.push(PathBuf::from(mp));
                }
            }
        }
        manifest_paths.sort();
        manifest_paths.dedup();

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
                let fingerprint = Self::compute_fingerprint(&workspace_root, &manifest_paths);
                return Ok(Self {
                    has_otel_shim_provider: has_shim,
                    shim_unsafe_packages: HashSet::new(),
                    host_only_packages: HashSet::new(),
                    host_only_manifest_dirs: HashSet::new(),
                    fingerprint,
                    manifest_paths,
                    workspace_root: Some(workspace_root),
                    package_manifest_dirs,
                    target_reachable_package_ids: HashSet::new(),
                    r4_otel_package_by_dependency: HashMap::new(),
                    r4_native_otel_artifacts: Vec::new(),
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

        // Identify workspace members that are internal dependencies of other workspace members.
        // A workspace member is an internal dependency if another member depends on it via a normal dependency edge.
        let mut internal_member_dep_ids: HashSet<String> = HashSet::new();
        for member_id in &workspace_members {
            if let Some(deps) = node_deps.get(member_id) {
                for (dep_id, kinds) in deps {
                    let is_normal_dep = kinds.iter().any(|k| k.is_none());
                    if is_normal_dep && workspace_members.contains(dep_id) {
                        internal_member_dep_ids.insert(dep_id.clone());
                    }
                }
            }
        }

        // Determine target roots: workspace members that produce standalone linked artifacts
        // (bin, cdylib) or are top-level crates (in-degree 0 among workspace members), excluding proc-macros.
        let default_target_roots: Vec<String> = {
            let mut roots: Vec<String> = workspace_members
                .iter()
                .filter(|id| !proc_macro_ids.contains(*id))
                .filter(|id| {
                    bin_or_cdylib_ids.contains(*id) || !internal_member_dep_ids.contains(*id)
                })
                .cloned()
                .collect();

            if roots.is_empty() {
                // If all members were filtered out, fall back to all non-proc-macro workspace members
                for id in &workspace_members {
                    if !proc_macro_ids.contains(id) {
                        roots.push(id.clone());
                    }
                }
            }

            if roots.is_empty() {
                // If all members are proc-macros or empty, use any non-proc-macro package
                for id in pkg_id_to_name.keys() {
                    if !proc_macro_ids.contains(id) {
                        roots.push(id.clone());
                    }
                }
            }
            roots
        };

        let target_roots: Vec<String> = if let Some(selected) = selected_package_ids {
            if !selected.is_empty() {
                let scoped: Vec<String> = selected
                    .iter()
                    .filter(|id| !proc_macro_ids.contains(*id))
                    .cloned()
                    .collect();
                if scoped.is_empty() {
                    default_target_roots
                } else {
                    scoped
                }
            } else {
                default_target_roots
            }
        } else {
            default_target_roots
        };

        // Per-target-root reachability BFS (G7).
        // A package is otel-shim-safe to instrument only if every target root whose reachable
        // set contains that package also has otel-shim in its own reachable set.
        let mut target_reachable_ids: HashSet<String> = HashSet::new();
        let mut target_root_reachability: Vec<HashSet<String>> = Vec::new();
        let mut shim_unsafe_packages: HashSet<String> = HashSet::new();
        let mut has_otel_shim_provider = false;

        for root in &target_roots {
            let mut root_reachable_ids: HashSet<String> = HashSet::new();
            let mut queue = std::collections::VecDeque::new();

            if root_reachable_ids.insert(root.clone()) {
                queue.push_back(root.clone());
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
                            && root_reachable_ids.insert(dep_id.clone())
                        {
                            queue.push_back(dep_id.clone());
                        }
                    }
                }
            }

            let root_has_shim = root_reachable_ids.iter().any(|id| {
                pkg_id_to_name
                    .get(id)
                    .map(|name| name == "otel-shim" || name == "otel_shim")
                    .unwrap_or(false)
            });

            if root_has_shim {
                has_otel_shim_provider = true;
            } else {
                // Any package reachable from a shim-less target root cannot safely receive
                // Tier-2 trampolines because this root will fail to link them (G7 / S11).
                for id in &root_reachable_ids {
                    if let Some(name) = pkg_id_to_name.get(id) {
                        shim_unsafe_packages.insert(name.replace('-', "_"));
                    }
                }
            }

            target_reachable_ids.extend(root_reachable_ids.iter().cloned());
            target_root_reachability.push(root_reachable_ids);
        }

        // R-4 native injection is safe only when every target root that shares a dependency
        // resolves exactly one, identical OpenTelemetry package id. Metadata package ids carry
        // the source/version identity; a disagreement is deliberately left unmapped.
        let mut r4_otel_package_by_dependency = HashMap::new();
        for dependency_id in &target_reachable_ids {
            let root_otel_ids: Vec<HashSet<String>> = target_root_reachability
                .iter()
                .filter(|reachable| reachable.contains(dependency_id))
                .map(|reachable| {
                    reachable
                        .iter()
                        .filter(|id| {
                            pkg_id_to_name
                                .get(*id)
                                .is_some_and(|name| name == "opentelemetry")
                        })
                        .cloned()
                        .collect()
                })
                .collect();
            if root_otel_ids.is_empty() || root_otel_ids.iter().any(|ids| ids.len() != 1) {
                continue;
            }
            let shared_otel_ids: HashSet<String> = root_otel_ids
                .iter()
                .flat_map(|ids| ids.iter().cloned())
                .collect();
            if shared_otel_ids.len() == 1 {
                r4_otel_package_by_dependency.insert(
                    dependency_id.clone(),
                    shared_otel_ids.into_iter().next().unwrap(),
                );
            }
        }

        // Target-reachable package names (normalized to underscored form for consistent comparison)
        let target_reachable_names: HashSet<String> = target_reachable_ids
            .iter()
            .filter_map(|id| pkg_id_to_name.get(id).cloned())
            .map(|n| n.replace('-', "_"))
            .collect();

        // Any package in the build graph whose name is NEVER reachable via target roots is host-only.
        // D2: Store normalized underscored names so rustc `--crate-name` matches without cross-collision.
        let host_only_packages =
            compute_host_only(pkg_id_to_name.values(), &target_reachable_names);

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

        let fingerprint = Self::compute_fingerprint(&workspace_root, &manifest_paths);

        Ok(Self {
            has_otel_shim_provider,
            shim_unsafe_packages,
            host_only_packages,
            host_only_manifest_dirs,
            fingerprint,
            manifest_paths,
            workspace_root: Some(workspace_root),
            package_manifest_dirs,
            target_reachable_package_ids: target_reachable_ids,
            r4_otel_package_by_dependency,
            r4_native_otel_artifacts: Vec::new(),
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

fn rustc_target(args: &[String]) -> Option<String> {
    args.iter()
        .position(|arg| arg == "--target")
        .and_then(|index| args.get(index + 1).cloned())
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix("--target=").map(String::from))
        })
}

fn rustc_profile(args: &[String]) -> R4Profile {
    let mut profile = R4Profile::default();
    for (index, arg) in args.iter().enumerate() {
        let codegen = if arg == "-C" {
            args.get(index + 1).map(String::as_str)
        } else {
            arg.strip_prefix("-C")
        };
        if let Some(codegen) = codegen {
            if let Some(value) = codegen.strip_prefix("opt-level=") {
                profile.opt_level = value.to_string();
            } else if let Some(value) = codegen.strip_prefix("overflow-checks=") {
                profile.overflow_checks = value == "on" || value == "true";
            } else if let Some(value) = codegen.strip_prefix("debug-assertions=") {
                profile.debug_assertions = value == "on" || value == "true";
            }
        }
        if arg == "--cfg"
            && args
                .get(index + 1)
                .is_some_and(|value| value == "debug_assertions")
            || arg == "--cfg=debug_assertions"
        {
            profile.debug_assertions = true;
        }
        if arg == "--test" {
            profile.test = true;
        }
    }
    profile
}

/// Compute the set of host-only package names by filtering out target-reachable packages.
///
/// Both target-reachable names and candidate package names are normalized to underscored
/// form so hyphenated and underscored representations compare equivalently without cross-pollution.
pub(crate) fn compute_host_only<S: AsRef<str>>(
    package_names: impl IntoIterator<Item = S>,
    target_reachable_names: &HashSet<String>,
) -> HashSet<String> {
    package_names
        .into_iter()
        .map(|n| n.as_ref().replace('-', "_"))
        .filter(|n| !target_reachable_names.contains(n))
        .collect()
}

/// Resolve a Cargo package specification (name, name:version, name@version, or exact id)
/// to an exact package ID present in `packages`.
pub fn resolve_package_spec(spec: &str, packages: &[serde_json::Value]) -> Result<String, String> {
    // 1. Direct package ID match
    for pkg in packages {
        if let Some(id) = pkg["id"].as_str() {
            if id == spec {
                return Ok(id.to_string());
            }
        }
    }

    // 2. Name with version qualifier: "name:version" or "name@version"
    let (name_part, version_part) = if let Some((n, v)) = spec.split_once(':') {
        (n, Some(v))
    } else if let Some((n, v)) = spec.split_once('@') {
        (n, Some(v))
    } else {
        (spec, None)
    };

    let mut matched_ids = Vec::new();
    for pkg in packages {
        let Some(id) = pkg["id"].as_str() else {
            continue;
        };
        let Some(name) = pkg["name"].as_str() else {
            continue;
        };
        let Some(version) = pkg["version"].as_str() else {
            continue;
        };

        if name == name_part || name.replace('-', "_") == name_part.replace('-', "_") {
            if let Some(req_ver) = version_part {
                if version == req_ver || version.starts_with(req_ver) {
                    matched_ids.push(id.to_string());
                }
            } else {
                matched_ids.push(id.to_string());
            }
        }
    }

    match matched_ids.len() {
        0 => Err(format!(
            "no package in metadata matches specification '{spec}'"
        )),
        1 => Ok(matched_ids.into_iter().next().unwrap()),
        _ => Err(format!(
            "package specification '{spec}' is ambiguous; matches: {:?}",
            matched_ids
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_d2_host_only_hyphenated_and_underscored_matching() {
        let mut plan = SessionPlan::default();
        // Since host_only_packages now stores only normalized underscored names:
        plan.host_only_packages.insert("pm_dep".to_string());

        assert!(plan.is_host_only("pm-dep"));
        assert!(plan.is_host_only("pm_dep"));
        assert!(plan.is_host_only_unit("pm_dep", None));
        assert!(plan.is_host_only_unit("pm-dep", None));
    }

    #[test]
    fn test_d2_normalization_does_not_cross_pollute_target_reachable() {
        let target_reachable_names: HashSet<String> = ["foo_bar".to_string()].into_iter().collect();
        let all_packages = ["foo-bar", "foo_bar"];

        // Calls the shared compute_host_only function directly
        let host_only_packages = compute_host_only(all_packages, &target_reachable_names);

        let plan = SessionPlan {
            host_only_packages,
            ..Default::default()
        };

        // foo_bar is target-reachable, so neither form should be marked host-only
        assert!(!plan.is_host_only("foo_bar"));
        assert!(!plan.is_host_only("foo-bar"));
        assert!(!plan.is_host_only_unit("foo_bar", None));
        assert!(!plan.is_host_only_unit("foo-bar", None));
    }

    #[test]
    fn test_d5_default_plan_disables_otel_shim_provider() {
        let plan = SessionPlan::default();
        assert!(
            !plan.has_otel_shim_provider(),
            "Default plan must default has_otel_shim_provider to false per S11 fail-open"
        );
    }

    #[test]
    fn test_d1_fingerprint_invalidation_on_manifest_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();

        let cargo_toml = root.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let manifests = vec![cargo_toml.clone()];
        let fp1 = SessionPlan::compute_fingerprint(root, &manifests);

        let plan = SessionPlan {
            fingerprint: fp1.clone(),
            manifest_paths: manifests.clone(),
            workspace_root: Some(root.to_path_buf()),
            ..Default::default()
        };

        assert!(plan.is_fresh(root));

        // Modify Cargo.toml
        fs::write(
            &cargo_toml,
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n[dependencies]\notel-shim = \"0.1\"\n",
        )
        .unwrap();

        assert!(
            !plan.is_fresh(root),
            "Plan must be rejected as stale when Cargo.toml is modified"
        );
    }

    #[test]
    fn test_g7_is_shim_unsafe_normalization() {
        let mut plan = SessionPlan::default();
        plan.shim_unsafe_packages.insert("shared_lib".to_string());

        assert!(plan.is_shim_unsafe("shared_lib"));
        assert!(plan.is_shim_unsafe("shared-lib"));
        assert!(!plan.is_shim_unsafe("other_lib"));
    }

    #[test]
    fn test_g7_mixed_provider_from_metadata_json() {
        let metadata = serde_json::json!({
            "packages": [
                {
                    "id": "app_a 0.1.0 (path+file:///app_a)",
                    "name": "app-a",
                    "manifest_path": "/app_a/Cargo.toml",
                    "targets": [{"kind": ["bin"], "name": "app-a"}]
                },
                {
                    "id": "app_b 0.1.0 (path+file:///app_b)",
                    "name": "app-b",
                    "manifest_path": "/app_b/Cargo.toml",
                    "targets": [{"kind": ["bin"], "name": "app-b"}]
                },
                {
                    "id": "common 0.1.0 (path+file:///common)",
                    "name": "common",
                    "manifest_path": "/common/Cargo.toml",
                    "targets": [{"kind": ["lib"], "name": "common"}]
                },
                {
                    "id": "otel-shim 0.1.0 (path+file:///otel-shim)",
                    "name": "otel-shim",
                    "manifest_path": "/otel-shim/Cargo.toml",
                    "targets": [{"kind": ["lib"], "name": "otel-shim"}]
                }
            ],
            "workspace_members": [
                "app_a 0.1.0 (path+file:///app_a)",
                "app_b 0.1.0 (path+file:///app_b)",
                "common 0.1.0 (path+file:///common)"
            ],
            "workspace_root": "/",
            "resolve": {
                "nodes": [
                    {
                        "id": "app_a 0.1.0 (path+file:///app_a)",
                        "deps": [
                            {"pkg": "common 0.1.0 (path+file:///common)", "dep_kinds": [{"kind": null}]},
                            {"pkg": "otel-shim 0.1.0 (path+file:///otel-shim)", "dep_kinds": [{"kind": null}]}
                        ]
                    },
                    {
                        "id": "app_b 0.1.0 (path+file:///app_b)",
                        "deps": [
                            {"pkg": "common 0.1.0 (path+file:///common)", "dep_kinds": [{"kind": null}]}
                        ]
                    },
                    {
                        "id": "common 0.1.0 (path+file:///common)",
                        "deps": []
                    },
                    {
                        "id": "otel-shim 0.1.0 (path+file:///otel-shim)",
                        "deps": []
                    }
                ]
            }
        });

        let plan = SessionPlan::from_metadata_json(&metadata).expect("parse metadata");
        assert!(
            plan.has_otel_shim_provider(),
            "Workspace has at least one root linking otel-shim"
        );
        assert!(
            plan.is_shim_unsafe("common"),
            "Shared dependency 'common' must be shim-unsafe because app_b lacks otel-shim"
        );
        assert!(
            plan.is_shim_unsafe("app-b"),
            "app_b lacks otel-shim and should be shim-unsafe"
        );
        assert!(
            !plan.is_shim_unsafe("app-a"),
            "app_a links otel-shim and must not be marked shim-unsafe"
        );
    }

    #[test]
    fn test_g7_single_binary_with_shim_from_metadata_json() {
        let metadata = serde_json::json!({
            "packages": [
                {
                    "id": "app_a 0.1.0 (path+file:///app_a)",
                    "name": "app-a",
                    "manifest_path": "/app_a/Cargo.toml",
                    "targets": [{"kind": ["bin"], "name": "app-a"}]
                },
                {
                    "id": "common 0.1.0 (path+file:///common)",
                    "name": "common",
                    "manifest_path": "/common/Cargo.toml",
                    "targets": [{"kind": ["lib"], "name": "common"}]
                },
                {
                    "id": "otel-shim 0.1.0 (path+file:///otel-shim)",
                    "name": "otel-shim",
                    "manifest_path": "/otel-shim/Cargo.toml",
                    "targets": [{"kind": ["lib"], "name": "otel-shim"}]
                }
            ],
            "workspace_members": [
                "app_a 0.1.0 (path+file:///app_a)",
                "common 0.1.0 (path+file:///common)"
            ],
            "workspace_root": "/",
            "resolve": {
                "nodes": [
                    {
                        "id": "app_a 0.1.0 (path+file:///app_a)",
                        "deps": [
                            {"pkg": "common 0.1.0 (path+file:///common)", "dep_kinds": [{"kind": null}]},
                            {"pkg": "otel-shim 0.1.0 (path+file:///otel-shim)", "dep_kinds": [{"kind": null}]}
                        ]
                    },
                    {
                        "id": "common 0.1.0 (path+file:///common)",
                        "deps": []
                    },
                    {
                        "id": "otel-shim 0.1.0 (path+file:///otel-shim)",
                        "deps": []
                    }
                ]
            }
        });

        let plan = SessionPlan::from_metadata_json(&metadata).expect("parse metadata");
        assert!(
            plan.has_otel_shim_provider(),
            "Workspace has target root with otel-shim"
        );
        assert!(
            !plan.is_shim_unsafe("common"),
            "In single-binary workspace, common is reached only by app_a and must be shim-safe"
        );
        assert!(
            !plan.is_shim_unsafe("app-a"),
            "app_a links otel-shim and is shim-safe"
        );
    }

    #[test]
    fn test_r4_multiple_otel_versions_across_target_roots_fail_open_for_shared_dependency() {
        let app_a = "app-a 0.1.0 (path+file:///app-a)";
        let app_b = "app-b 0.1.0 (path+file:///app-b)";
        let common = "common 0.1.0 (path+file:///common)";
        let otel_30 = "registry+https://example.invalid#index#opentelemetry@0.30.0";
        let otel_32 = "registry+https://example.invalid#index#opentelemetry@0.32.0";
        let metadata = serde_json::json!({
            "packages": [
                {"id": app_a, "name": "app-a", "version": "0.1.0", "manifest_path": "/app-a/Cargo.toml", "targets": [{"kind": ["bin"]}]},
                {"id": app_b, "name": "app-b", "version": "0.1.0", "manifest_path": "/app-b/Cargo.toml", "targets": [{"kind": ["bin"]}]},
                {"id": common, "name": "common", "version": "0.1.0", "manifest_path": "/common/Cargo.toml", "targets": [{"kind": ["lib"]}]},
                {"id": otel_30, "name": "opentelemetry", "version": "0.30.0", "manifest_path": "/registry/opentelemetry-0.30.0/Cargo.toml", "targets": [{"kind": ["lib"]}]},
                {"id": otel_32, "name": "opentelemetry", "version": "0.32.0", "manifest_path": "/registry/opentelemetry-0.32.0/Cargo.toml", "targets": [{"kind": ["lib"]}]}
            ],
            "workspace_members": [app_a, app_b, common],
            "workspace_root": "/",
            "resolve": {"nodes": [
                {"id": app_a, "deps": [{"pkg": common, "dep_kinds": [{"kind": null}]}, {"pkg": otel_32, "dep_kinds": [{"kind": null}]}]},
                {"id": app_b, "deps": [{"pkg": common, "dep_kinds": [{"kind": null}]}, {"pkg": otel_30, "dep_kinds": [{"kind": null}]}]},
                {"id": common, "deps": []},
                {"id": otel_30, "deps": [], "features": ["trace"]},
                {"id": otel_32, "deps": [], "features": ["trace"]}
            ]}
        });

        let plan = SessionPlan::from_metadata_json(&metadata).expect("parse multi-root metadata");
        assert!(
            !plan.r4_otel_package_by_dependency.contains_key(common),
            "a shared dependency reached by roots with 0.30 and 0.32 must be left on the fail-open path"
        );
        assert_eq!(
            plan.r4_otel_package_by_dependency.get(app_a),
            Some(&otel_32.to_string()),
            "a single-root package retains its exact 0.32 package identity"
        );
        assert_eq!(
            plan.r4_otel_package_by_dependency.get(app_b),
            Some(&otel_30.to_string()),
            "a single-root package retains its exact 0.30 package identity"
        );
    }

    #[test]
    fn test_r4_compiler_artifact_requires_exact_target_and_profile() {
        let temp = tempfile::tempdir().expect("create artifact fixture");
        let dep_dir = temp.path().join("dep");
        fs::create_dir_all(&dep_dir).unwrap();
        let source = dep_dir.join("src/lib.rs");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "pub fn work() {}\n").unwrap();
        let rlib = temp.path().join("libopentelemetry-exact.rlib");
        fs::write(&rlib, b"artifact").unwrap();

        let dep_id = "dep 0.1.0 (path+file:///dep)";
        let otel_id = "registry+https://example.invalid#index#opentelemetry@0.32.0";
        let metadata = serde_json::json!({
            "packages": [
                {"id": dep_id, "name": "dep", "version": "0.1.0"},
                {"id": otel_id, "name": "opentelemetry", "version": "0.32.0"}
            ],
            "resolve": {"nodes": [
                {"id": otel_id, "features": ["trace", "testing"]}
            ]}
        });
        let mut plan = SessionPlan::default();
        plan.package_manifest_dirs
            .insert(dep_id.to_string(), dep_dir);
        plan.r4_otel_package_by_dependency
            .insert(dep_id.to_string(), otel_id.to_string());
        let message = serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": otel_id,
            "filenames": [rlib],
            "profile": {"opt_level": "0", "debug_assertions": true, "overflow_checks": true, "test": false}
        });
        plan.add_r4_artifacts_from_cargo_json(&metadata, format!("{message}\n").as_bytes(), None)
            .expect("accept exact compiler artifact");

        assert!(plan
            .r4_native_otel_artifact_for(&source, &[])
            .expect("resolve default dev profile")
            .is_some());
        assert!(
            plan.r4_native_otel_artifact_for(
                &source,
                &["--target".to_string(), "wasm32-wasip1".to_string()],
            )
            .is_err(),
            "a host artifact must not be injected into a target unit"
        );
        assert!(
            plan.r4_native_otel_artifact_for(
                &source,
                &["-C".to_string(), "opt-level=3".to_string()],
            )
            .is_err(),
            "a dev artifact must not be injected into a release-profile unit"
        );
    }

    #[test]
    fn test_resolve_package_spec_handling() {
        let packages = serde_json::json!([
            {"id": "app-a 0.1.0 (path+file:///app_a)", "name": "app-a", "version": "0.1.0"},
            {"id": "app-b 0.2.0 (path+file:///app_b)", "name": "app-b", "version": "0.2.0"},
            {"id": "app-dup 0.1.0 (path+file:///dup1)", "name": "app-dup", "version": "0.1.0"},
            {"id": "app-dup 0.2.0 (path+file:///dup2)", "name": "app-dup", "version": "0.2.0"},
        ]);
        let pkgs = packages.as_array().unwrap();

        // Exact match by name
        assert_eq!(
            resolve_package_spec("app-a", pkgs).unwrap(),
            "app-a 0.1.0 (path+file:///app_a)"
        );
        // Normalized name
        assert_eq!(
            resolve_package_spec("app_a", pkgs).unwrap(),
            "app-a 0.1.0 (path+file:///app_a)"
        );
        // Exact match by ID
        assert_eq!(
            resolve_package_spec("app-b 0.2.0 (path+file:///app_b)", pkgs).unwrap(),
            "app-b 0.2.0 (path+file:///app_b)"
        );
        // Version qualified with ':'
        assert_eq!(
            resolve_package_spec("app-dup:0.1.0", pkgs).unwrap(),
            "app-dup 0.1.0 (path+file:///dup1)"
        );
        // Version qualified with '@'
        assert_eq!(
            resolve_package_spec("app-dup@0.2.0", pkgs).unwrap(),
            "app-dup 0.2.0 (path+file:///dup2)"
        );
        // Ambiguous without version
        assert!(resolve_package_spec("app-dup", pkgs).is_err());
        // Unknown package
        assert!(resolve_package_spec("unknown-pkg", pkgs).is_err());
    }

    #[test]
    fn test_from_metadata_json_scoped_isolates_unrelated_target_roots() {
        let metadata = serde_json::json!({
            "packages": [
                {
                    "id": "app-a 0.1.0 (path+file:///app_a)",
                    "name": "app-a",
                    "version": "0.1.0",
                    "manifest_path": "/app_a/Cargo.toml",
                    "targets": [{"kind": ["bin"]}]
                },
                {
                    "id": "app-b 0.1.0 (path+file:///app_b)",
                    "name": "app-b",
                    "version": "0.1.0",
                    "manifest_path": "/app_b/Cargo.toml",
                    "targets": [{"kind": ["bin"]}]
                },
                {
                    "id": "common 0.1.0 (path+file:///common)",
                    "name": "common",
                    "version": "0.1.0",
                    "manifest_path": "/common/Cargo.toml",
                    "targets": [{"kind": ["lib"]}]
                },
                {
                    "id": "otel-shim 0.1.0 (path+file:///otel-shim)",
                    "name": "otel-shim",
                    "version": "0.1.0",
                    "manifest_path": "/otel-shim/Cargo.toml",
                    "targets": [{"kind": ["lib"]}]
                }
            ],
            "workspace_members": [
                "app-a 0.1.0 (path+file:///app_a)",
                "app-b 0.1.0 (path+file:///app_b)"
            ],
            "workspace_root": "/",
            "resolve": {
                "nodes": [
                    {
                        "id": "app-a 0.1.0 (path+file:///app_a)",
                        "deps": [
                            {"pkg": "common 0.1.0 (path+file:///common)", "dep_kinds": [{"kind": null}]},
                            {"pkg": "otel-shim 0.1.0 (path+file:///otel-shim)", "dep_kinds": [{"kind": null}]}
                        ]
                    },
                    {
                        "id": "app-b 0.1.0 (path+file:///app_b)",
                        "deps": [
                            {"pkg": "common 0.1.0 (path+file:///common)", "dep_kinds": [{"kind": null}]}
                            // app-b intentionally lacks otel-shim
                        ]
                    },
                    {
                        "id": "common 0.1.0 (path+file:///common)",
                        "deps": []
                    },
                    {
                        "id": "otel-shim 0.1.0 (path+file:///otel-shim)",
                        "deps": []
                    }
                ]
            }
        });

        // 1. Unscoped: both app-a and app-b are roots. Because app-b lacks otel-shim, common is marked shim_unsafe.
        let unscoped_plan = SessionPlan::from_metadata_json(&metadata).unwrap();
        assert!(unscoped_plan.is_shim_unsafe("common"));

        // 2. Scoped to app-a: only app-a is root. app-a has otel-shim, so common is NOT shim_unsafe.
        let selected: HashSet<String> = ["app-a 0.1.0 (path+file:///app_a)".to_string()]
            .into_iter()
            .collect();
        let scoped_plan =
            SessionPlan::from_metadata_json_scoped(&metadata, Some(&selected)).unwrap();
        assert!(!scoped_plan.is_shim_unsafe("common"));
        assert!(scoped_plan
            .target_reachable_package_ids
            .contains("common 0.1.0 (path+file:///common)"));
        assert!(!scoped_plan
            .target_reachable_package_ids
            .contains("app-b 0.1.0 (path+file:///app_b)"));
    }
}
