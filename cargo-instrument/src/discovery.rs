use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("Missing rustc arguments")]
    EmptyArguments,
    #[error("Could not determine primary source file from arguments: {0:?}")]
    MissingSourceFile(Vec<String>),
}

/// The classification of a rustc compiler invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompilationUnit {
    /// An ordinary application or dependency Rust crate compilation.
    /// This is the primary target eligible for compile-time AST analysis.
    RustCrate {
        crate_name: String,
        crate_types: Vec<String>,
        edition: Option<String>,
        target: Option<String>,
        out_dir: Option<PathBuf>,
        source_file: PathBuf,
        is_test: bool,
        has_opentelemetry: bool,
    },

    /// A build script compilation (e.g. `build_script_build` or `build.rs`).
    /// Passed through without instrumentation per Phase 0 architecture.
    BuildScript {
        crate_name: String,
        source_file: PathBuf,
    },

    /// A procedural macro compilation (`--crate-type proc-macro`).
    /// Passed through without instrumentation (host-only compiler extension).
    ProcMacro {
        crate_name: String,
        source_file: PathBuf,
    },

    /// A compiler query invocation (e.g. `rustc -vV`, `rustc --print=sysroot`, `rustc -`).
    /// Passed through immediately.
    CompilerQuery { query_flags: Vec<String> },

    /// An invocation that does not match standard crate compilation or has no Rust source.
    /// Passed through safely.
    PassThrough { reason: String },
}

impl CompilationUnit {
    /// Whether this compilation unit represents a crate eligible for AST candidate analysis.
    pub fn is_eligible_for_analysis(&self) -> bool {
        matches!(self, CompilationUnit::RustCrate { .. })
    }

    /// The name of the crate being compiled, if available.
    pub fn crate_name(&self) -> Option<&str> {
        match self {
            CompilationUnit::RustCrate { crate_name, .. } => Some(crate_name),
            CompilationUnit::BuildScript { crate_name, .. } => Some(crate_name),
            CompilationUnit::ProcMacro { crate_name, .. } => Some(crate_name),
            _ => None,
        }
    }

    /// The path to the primary source file, if available.
    pub fn source_file(&self) -> Option<&Path> {
        match self {
            CompilationUnit::RustCrate { source_file, .. } => Some(source_file.as_path()),
            CompilationUnit::BuildScript { source_file, .. } => Some(source_file.as_path()),
            CompilationUnit::ProcMacro { source_file, .. } => Some(source_file.as_path()),
            _ => None,
        }
    }

    /// Path to the compilation output directory, if available.
    pub fn out_dir(&self) -> Option<&Path> {
        match self {
            CompilationUnit::RustCrate { out_dir, .. } => out_dir.as_deref(),
            _ => None,
        }
    }

    /// Whether the compilation unit includes opentelemetry as an extern dependency.
    pub fn has_opentelemetry(&self) -> bool {
        match self {
            CompilationUnit::RustCrate {
                has_opentelemetry, ..
            } => *has_opentelemetry,
            _ => false,
        }
    }
}

/// Parsed representation of a rustc invocation received from Cargo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateInvocation {
    /// The classified compilation unit.
    pub unit: CompilationUnit,
    /// The full, original arguments provided to rustc (unmodified).
    pub original_args: Vec<String>,
}

impl CrateInvocation {
    /// Parse and classify rustc arguments received by the wrapper.
    pub fn parse(args: &[String]) -> Result<Self, DiscoveryError> {
        if args.is_empty() {
            return Err(DiscoveryError::EmptyArguments);
        }

        let original_args = args.to_vec();

        // 1. Check for compiler query flags (e.g. -vV, --version, --print)
        let query_flags: Vec<String> = args
            .iter()
            .filter(|a| {
                *a == "-vV"
                    || *a == "-V"
                    || *a == "--version"
                    || a.starts_with("--print")
                    || *a == "-"
            })
            .cloned()
            .collect();

        // If there's an explicit query flag, or if the sole input is "-" (stdin), it's a query
        if !query_flags.is_empty()
            && (args
                .iter()
                .any(|a| *a == "-vV" || *a == "--version" || *a == "-V")
                || args.iter().any(|a| *a == "-")
                || args.iter().all(|a| a.starts_with('-')))
        {
            return Ok(CrateInvocation {
                unit: CompilationUnit::CompilerQuery { query_flags },
                original_args,
            });
        }

        // 2. Parse command-line flags and options
        let mut crate_name: Option<String> = None;
        let mut crate_types: Vec<String> = Vec::new();
        let mut edition: Option<String> = None;
        let mut target: Option<String> = None;
        let mut out_dir: Option<PathBuf> = None;
        let mut is_test = false;
        let mut has_opentelemetry = false;
        let mut positional_source: Option<PathBuf> = None;

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];

            if arg == "--test" {
                is_test = true;
                i += 1;
                continue;
            }

            if let Some(val) = extract_opt_value(args, &mut i, "--crate-name") {
                crate_name = Some(val);
                continue;
            }

            if let Some(val) = extract_opt_value(args, &mut i, "--crate-type") {
                crate_types.push(val);
                continue;
            }

            if let Some(val) = extract_opt_value(args, &mut i, "--edition") {
                edition = Some(val);
                continue;
            }

            if let Some(val) = extract_opt_value(args, &mut i, "--target") {
                target = Some(val);
                continue;
            }

            if let Some(val) = extract_opt_value(args, &mut i, "--out-dir") {
                out_dir = Some(PathBuf::from(val));
                continue;
            }

            if arg == "--extern" {
                if i + 1 < args.len() {
                    let spec = &args[i + 1];
                    let extern_name = spec.split('=').next().unwrap_or(spec);
                    let extern_crate = extern_name.rsplit(':').next().unwrap_or(extern_name);
                    if extern_crate == "opentelemetry" {
                        has_opentelemetry = true;
                    }
                    i += 2;
                    continue;
                }
            } else if let Some(spec) = arg.strip_prefix("--extern=") {
                let extern_name = spec.split('=').next().unwrap_or(spec);
                let extern_crate = extern_name.rsplit(':').next().unwrap_or(extern_name);
                if extern_crate == "opentelemetry" {
                    has_opentelemetry = true;
                }
                i += 1;
                continue;
            }

            // Skip options that consume the next argument
            if is_argument_consuming_flag(arg) {
                i += 2; // skip flag and its parameter
                continue;
            }

            // Skip other known flags
            if arg.starts_with('-') {
                i += 1;
                continue;
            }

            // Positional argument: Check if it looks like a Rust source file (.rs)
            if arg.ends_with(".rs") {
                positional_source = Some(PathBuf::from(arg));
            }

            i += 1;
        }

        // If no source file was found, but query flags were present
        if positional_source.is_none() && !query_flags.is_empty() {
            return Ok(CrateInvocation {
                unit: CompilationUnit::CompilerQuery { query_flags },
                original_args,
            });
        }

        // Must have a source file to classify as a crate
        let source_file = match positional_source {
            Some(p) => p,
            None => {
                return Ok(CrateInvocation {
                    unit: CompilationUnit::PassThrough {
                        reason: "No .rs source file specified in rustc arguments".to_string(),
                    },
                    original_args,
                });
            }
        };

        let resolved_crate_name = crate_name.unwrap_or_else(|| {
            source_file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        });

        // 3. Classify crate type
        // A. Proc macro
        if crate_types.iter().any(|t| t == "proc-macro") {
            return Ok(CrateInvocation {
                unit: CompilationUnit::ProcMacro {
                    crate_name: resolved_crate_name,
                    source_file,
                },
                original_args,
            });
        }

        // B. Build script
        let is_build_script_name = resolved_crate_name == "build_script_build"
            || resolved_crate_name.starts_with("build_script_");
        let is_build_rs_file = source_file
            .file_name()
            .and_then(|f| f.to_str())
            .map(|f| f == "build.rs")
            .unwrap_or(false);

        if is_build_script_name || is_build_rs_file {
            return Ok(CrateInvocation {
                unit: CompilationUnit::BuildScript {
                    crate_name: resolved_crate_name,
                    source_file,
                },
                original_args,
            });
        }

        // C. Ordinary Rust crate (eligible)
        Ok(CrateInvocation {
            unit: CompilationUnit::RustCrate {
                crate_name: resolved_crate_name,
                crate_types,
                edition,
                target,
                out_dir,
                source_file,
                is_test,
                has_opentelemetry,
            },
            original_args,
        })
    }
}

/// Helper to extract value from `--flag value` or `--flag=value`.
fn extract_opt_value(args: &[String], idx: &mut usize, flag: &str) -> Option<String> {
    let arg = &args[*idx];
    if arg == flag {
        if *idx + 1 < args.len() {
            let val = args[*idx + 1].clone();
            *idx += 2;
            return Some(val);
        }
    } else if let Some(stripped) = arg.strip_prefix(&format!("{flag}=")) {
        let val = stripped.to_string();
        *idx += 1;
        return Some(val);
    }
    None
}

/// Flags in rustc that consume the following argument.
fn is_argument_consuming_flag(arg: &str) -> bool {
    matches!(
        arg,
        "-o" | "-L"
            | "-C"
            | "-A"
            | "-W"
            | "-D"
            | "-F"
            | "--color"
            | "--error-format"
            | "--json"
            | "--emit"
            | "--cap-lints"
            | "--explain"
            | "--check-cfg"
            | "--remap-path-prefix"
    )
}
