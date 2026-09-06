use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serial_test::serial;
use sha2::{Digest, Sha256};

use cargo_instrument::ast::{analyze_source_file, analyze_source_str, count_crate_functions};
use cargo_instrument::candidate::UnsafePolicy;
use cargo_instrument::transform::transform_source_str_with_trampoline;

fn find_cargo_home() -> PathBuf {
    std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .expect("valid home directory");
            PathBuf::from(home).join(".cargo")
        })
}

fn find_registry_crate_dir(crate_prefix: &str) -> Option<PathBuf> {
    let cargo_home = find_cargo_home();
    let registry_src = cargo_home.join("registry").join("src");
    if registry_src.exists() {
        if let Ok(entries) = fs::read_dir(&registry_src) {
            for entry in entries.flatten() {
                if let Ok(subentries) = fs::read_dir(entry.path()) {
                    for sub in subentries.flatten() {
                        let name = sub.file_name().to_string_lossy().to_string();
                        if name.starts_with(crate_prefix) && sub.path().is_dir() {
                            return Some(sub.path());
                        }
                    }
                }
            }
        }
    }
    None
}

// ----------------------------------------------------------------------------
// A4–A8: End-to-End Runtime Telemetry on Real Registry Dependency (census)
// ----------------------------------------------------------------------------

#[test]
#[serial]
#[ignore = "requires CARGO_INSTRUMENT_REGISTRY=1 and cached census-0.4.2"]
fn test_e2e_census_runtime_telemetry() {
    // Assert explicit opt-in gate per M2 when explicitly invoked with --ignored
    assert!(
        std::env::var("CARGO_INSTRUMENT_REGISTRY").is_ok(),
        "CARGO_INSTRUMENT_REGISTRY=1 must be set to run registry e2e tests"
    );

    let census_dir = find_registry_crate_dir("census-0.4.2")
        .expect("census-0.4.2 must be cached in registry src cache to run e2e telemetry test");
    assert!(census_dir.exists());

    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let ws_root = temp_dir.path();

    let current_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = current_manifest
        .parent()
        .expect("cargo-instrument parent is repo root");
    let otel_shim_path = repo_root.join("otel-shim");
    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    let otel_shim_path_escaped = otel_shim_path.to_string_lossy().replace('\\', "/");

    let app_dir = ws_root.join("census_e2e_app");
    let app_src = app_dir.join("src");
    fs::create_dir_all(&app_src).expect("create app src");

    let app_cargo = app_dir.join("Cargo.toml");
    fs::write(
        &app_cargo,
        format!(
            r#"[package]
name = "census_e2e_app"
version = "0.1.0"
edition = "2021"

[dependencies]
census = "=0.4.2"
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
otel-shim = {{ path = "{otel_shim_path_escaped}" }}
"#
        ),
    )
    .expect("write Cargo.toml");

    let app_main = app_src.join("main.rs");
    fs::write(
        &app_main,
        r#"use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use opentelemetry::trace::{SpanKind, Status, TracerProvider};

fn main() {
    // S10 / E-10: Runtime initialization
    otel_shim::init();

    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());

    app_workflow();

    // A8: Zero leaked handles
    assert_eq!(
        otel_shim::active_span_count(),
        0,
        "active_span_count must be 0 after scenario completion (A8)"
    );

    let spans = exporter.get_finished_spans().expect("finished spans");
    println!("TOTAL_EXPORTED_SPANS={}", spans.len());
    for s in &spans {
        println!("SPAN: name='{}' kind={:?} status={:?} parent={:?}", s.name, s.span_kind, s.status, s.parent_span_id);
    }

    // A5: Verified exported spans from application and dependency
    let app_span = spans.iter().find(|s| s.name == "app_workflow").expect("app_workflow span");
    let new_span = spans.iter().find(|s| s.name == "Inventory<T>::new").expect("Inventory<T>::new span");
    let track_span = spans.iter().find(|s| s.name == "Inventory<T>::track").expect("Inventory<T>::track span");
    let list_span = spans.iter().find(|s| s.name == "Inventory<T>::list").expect("Inventory<T>::list span");

    assert_eq!(new_span.span_kind, SpanKind::Internal);
    assert_eq!(track_span.span_kind, SpanKind::Internal);
    assert_eq!(list_span.span_kind, SpanKind::Internal);

    // A6: Cross-crate parenting: dependency spans must have app caller's span_id as parent_span_id
    assert_eq!(
        new_span.parent_span_id,
        app_span.span_context.span_id(),
        "Inventory<T>::new parent must be app_workflow (A6)"
    );
    assert_eq!(
        track_span.parent_span_id,
        app_span.span_context.span_id(),
        "Inventory<T>::track parent must be app_workflow (A6)"
    );
    assert_eq!(
        list_span.parent_span_id,
        app_span.span_context.span_id(),
        "Inventory<T>::list parent must be app_workflow (A6)"
    );

    // A7: Status unset on success
    assert_eq!(new_span.status, Status::Unset);
    assert_eq!(track_span.status, Status::Unset);
    assert_eq!(list_span.status, Status::Unset);

    println!("E2E_CENSUS_SUCCESS");
}

pub fn app_workflow() {
    let inventory = census::Inventory::new();
    let tracked = inventory.track(42);
    let _list = inventory.list();
    drop(tracked);
}
"#,
    )
    .expect("write main.rs");

    let target_dir = ws_root.join("target").join("instrumented");

    let output = Command::new("cargo")
        .args([
            "run",
            "--manifest-path",
            app_cargo.to_str().unwrap(),
            "--target-dir",
            target_dir.to_str().unwrap(),
        ])
        .env("RUSTC_WRAPPER", cargo_instrument_bin)
        .env("CARGO_INSTRUMENT_REGISTRY", "1")
        .output()
        .expect("execute cargo run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "E2E cargo run failed!\nSTDOUT:\n{stdout}\nSTDERR:\n{stderr}"
    );

    assert!(
        stdout.contains("E2E_CENSUS_SUCCESS"),
        "Execution did not reach expected success marker!\nSTDOUT:\n{stdout}"
    );
}

// ----------------------------------------------------------------------------
// A9: Dependency Coverage & Exact Reconciliation Table
// ----------------------------------------------------------------------------

#[test]
#[ignore = "requires CARGO_INSTRUMENT_REGISTRY=1 and cached registry crates"]
fn test_dependency_coverage_and_reconciliation_table() {
    // Assert explicit opt-in gate per M2 when explicitly invoked with --ignored
    assert!(
        std::env::var("CARGO_INSTRUMENT_REGISTRY").is_ok(),
        "CARGO_INSTRUMENT_REGISTRY=1 must be set to run reconciliation table test"
    );

    struct SampleCrate {
        name: &'static str,
        path: PathBuf,
    }

    let census_dir = find_registry_crate_dir("census-0.4.2")
        .expect("census-0.4.2 must be cached in registry src cache");
    let async_trait_dir = find_registry_crate_dir("async-trait-0.1")
        .expect("async-trait-0.1 must be cached in registry src cache");

    let samples = vec![
        SampleCrate {
            name: "census-0.4.2",
            path: census_dir.join("src").join("lib.rs"),
        },
        SampleCrate {
            name: "async-trait",
            path: async_trait_dir.join("src").join("lib.rs"),
        },
    ];

    println!("\n=== A9 DEPENDENCY ELIGIBILITY & RECONCILIATION TABLE ===");
    println!("{:<20} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8}",
        "Crate", "Total", "Eligible", "Inline", "Adapter", "Drop", "CfgTest", "Const", "Extern", "SelfRec", "Otel", "Nested"
    );
    println!("{:-<150}", "");

    for sample in &samples {
        assert!(
            sample.path.exists(),
            "Sample crate path must exist: {:?}",
            sample.path
        );
        let content = fs::read_to_string(&sample.path).expect("read sample crate source");
        let syn_file = syn::parse_file(&content).expect("parse sample crate file");
        let total_fns = count_crate_functions(&sample.path, &syn_file);

        let report = analyze_source_file(sample.name, &sample.path).expect("analyze source file");

        // Universal Reconciliation Identity
        let detected = report.candidates.len();
        let skipped_sum = report.skipped_stats.total();

        assert_eq!(
            detected + skipped_sum,
            total_fns,
            "Reconciliation invariant failed for {}! candidates={} skipped={:?} total_fns={}",
            sample.name,
            detected,
            report.skipped_stats,
            total_fns
        );

        println!("{:<20} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8} | {:<8}",
            sample.name,
            total_fns,
            detected,
            report.skipped_stats.inline_attribute,
            report.skipped_stats.adapter_trait,
            report.skipped_stats.drop_implementation,
            report.skipped_stats.cfg_test,
            report.skipped_stats.const_fn,
            report.skipped_stats.extern_abi,
            report.skipped_stats.self_recursive,
            report.skipped_stats.handwritten_otel,
            report.skipped_stats.nested_function,
        );
    }
}

// ----------------------------------------------------------------------------
// A18: Byte Reproducibility of Generated Mirrors
// ----------------------------------------------------------------------------

#[test]
#[ignore = "requires CARGO_INSTRUMENT_REGISTRY=1 and cached registry crates"]
fn test_mirror_byte_reproducibility() {
    // Assert explicit opt-in gate per M2 when explicitly invoked with --ignored
    assert!(
        std::env::var("CARGO_INSTRUMENT_REGISTRY").is_ok(),
        "CARGO_INSTRUMENT_REGISTRY=1 must be set to run reproducibility test"
    );

    let census_dir = find_registry_crate_dir("census-0.4.2")
        .expect("census-0.4.2 must be cached in registry src cache to run reproducibility test");

    let lib_rs = census_dir.join("src").join("lib.rs");
    let content = fs::read_to_string(&lib_rs).expect("read census lib.rs");

    let report = analyze_source_str("census", &lib_rs, &content).expect("analyze census");
    assert!(!report.candidates.is_empty());

    // Run Pass 1
    let pass1 = transform_source_str_with_trampoline(
        &content,
        "census",
        Some("2021".to_string()),
        UnsafePolicy::Allowed,
        &report.candidates,
    )
    .expect("pass 1 transformation");

    // Run Pass 2
    let pass2 = transform_source_str_with_trampoline(
        &content,
        "census",
        Some("2021".to_string()),
        UnsafePolicy::Allowed,
        &report.candidates,
    )
    .expect("pass 2 transformation");

    let mut hasher1 = Sha256::new();
    hasher1.update(pass1.as_bytes());
    let hash1 = format!("{:x}", hasher1.finalize());

    let mut hasher2 = Sha256::new();
    hasher2.update(pass2.as_bytes());
    let hash2 = format!("{:x}", hasher2.finalize());

    assert_eq!(
        hash1, hash2,
        "A18 Reproducibility failed: Pass 1 and Pass 2 produced different SHA-256 hashes!"
    );
    assert_eq!(
        pass1, pass2,
        "A18 Reproducibility failed: Pass 1 and Pass 2 produced different bytes!"
    );
}
