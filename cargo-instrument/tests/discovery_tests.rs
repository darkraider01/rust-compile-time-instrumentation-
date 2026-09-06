use cargo_instrument::discovery::{CompilationUnit, CrateInvocation};
use std::path::PathBuf;

#[test]
fn test_classify_ordinary_rust_crate() {
    let args = vec![
        "--crate-name".to_string(),
        "my_service".to_string(),
        "--edition=2021".to_string(),
        "src/main.rs".to_string(),
        "--crate-type".to_string(),
        "bin".to_string(),
        "--out-dir".to_string(),
        "target/debug/deps".to_string(),
    ];

    let invocation = CrateInvocation::parse(&args).expect("parsing should succeed");
    match &invocation.unit {
        CompilationUnit::RustCrate {
            crate_name,
            crate_types,
            edition,
            source_file,
            is_test,
            ..
        } => {
            assert_eq!(crate_name, "my_service");
            assert_eq!(crate_types, &vec!["bin"]);
            assert_eq!(edition.as_deref(), Some("2021"));
            assert_eq!(source_file, &PathBuf::from("src/main.rs"));
            assert!(!is_test);
            assert!(invocation.unit.is_eligible_for_analysis());
        }
        other => panic!("expected RustCrate, got {:?}", other),
    }
}

#[test]
fn test_classify_proc_macro() {
    let args = vec![
        "--crate-name".to_string(),
        "my_macros".to_string(),
        "src/lib.rs".to_string(),
        "--crate-type".to_string(),
        "proc-macro".to_string(),
    ];

    let invocation = CrateInvocation::parse(&args).expect("parsing should succeed");
    match &invocation.unit {
        CompilationUnit::ProcMacro {
            crate_name,
            source_file,
        } => {
            assert_eq!(crate_name, "my_macros");
            assert_eq!(source_file, &PathBuf::from("src/lib.rs"));
            assert!(!invocation.unit.is_eligible_for_analysis());
        }
        other => panic!("expected ProcMacro, got {:?}", other),
    }
}

#[test]
fn test_classify_build_script() {
    let args = vec![
        "--crate-name".to_string(),
        "build_script_build".to_string(),
        "build.rs".to_string(),
        "--crate-type".to_string(),
        "bin".to_string(),
    ];

    let invocation = CrateInvocation::parse(&args).expect("parsing should succeed");
    match &invocation.unit {
        CompilationUnit::BuildScript {
            crate_name,
            source_file,
        } => {
            assert_eq!(crate_name, "build_script_build");
            assert_eq!(source_file, &PathBuf::from("build.rs"));
            assert!(!invocation.unit.is_eligible_for_analysis());
        }
        other => panic!("expected BuildScript, got {:?}", other),
    }
}

#[test]
fn test_classify_compiler_queries() {
    // Version query
    let args_v = vec!["-vV".to_string()];
    let inv_v = CrateInvocation::parse(&args_v).expect("parsing should succeed");
    assert!(matches!(inv_v.unit, CompilationUnit::CompilerQuery { .. }));
    assert!(!inv_v.unit.is_eligible_for_analysis());

    // Print query
    let args_p = vec![
        "-".to_string(),
        "--crate-name".to_string(),
        "___".to_string(),
        "--print=file-names".to_string(),
        "--crate-type".to_string(),
        "bin".to_string(),
    ];
    let inv_p = CrateInvocation::parse(&args_p).expect("parsing should succeed");
    assert!(matches!(inv_p.unit, CompilationUnit::CompilerQuery { .. }));
    assert!(!inv_p.unit.is_eligible_for_analysis());
}

#[test]
fn test_paths_containing_spaces_and_backslashes() {
    let args = vec![
        "--crate-name".to_string(),
        "spaced_app".to_string(),
        "C:\\My Code\\Project Alpha\\src\\main.rs".to_string(),
        "--out-dir".to_string(),
        "C:\\My Code\\Project Alpha\\target\\debug".to_string(),
    ];

    let invocation = CrateInvocation::parse(&args).expect("parsing should succeed");
    match invocation.unit {
        CompilationUnit::RustCrate {
            crate_name,
            source_file,
            out_dir,
            ..
        } => {
            assert_eq!(crate_name, "spaced_app");
            assert_eq!(
                source_file,
                PathBuf::from("C:\\My Code\\Project Alpha\\src\\main.rs")
            );
            assert_eq!(
                out_dir,
                Some(PathBuf::from("C:\\My Code\\Project Alpha\\target\\debug"))
            );
        }
        other => panic!("expected RustCrate, got {:?}", other),
    }
}

#[test]
fn test_classify_real_cargo_build_invocation() {
    let args = vec![
        "rustc".to_string(),
        "--crate-name".to_string(),
        "cargo_instrument".to_string(),
        "--edition=2021".to_string(),
        "src/main.rs".to_string(),
        "--error-format=json".to_string(),
        "--json=diagnostic-rendered-ansi,artifacts,future-incompat".to_string(),
        "--diagnostic-width=120".to_string(),
        "--crate-type".to_string(),
        "bin".to_string(),
        "--emit=dep-info,link".to_string(),
        "-C".to_string(),
        "embed-bitcode=no".to_string(),
        "-C".to_string(),
        "debuginfo=2".to_string(),
        "-C".to_string(),
        "split-debuginfo=unpacked".to_string(),
        "--check-cfg".to_string(),
        "cfg(docsrs,test)".to_string(),
        "-C".to_string(),
        "metadata=701614f19eecece4".to_string(),
        "-C".to_string(),
        "extra-filename=-701614f19eecece4".to_string(),
        "--out-dir".to_string(),
        "target/debug/deps".to_string(),
        "-L".to_string(),
        "dependency=target/debug/deps".to_string(),
    ];

    let invocation = CrateInvocation::parse(&args).expect("parsing real cargo argv must succeed");
    match &invocation.unit {
        CompilationUnit::RustCrate {
            crate_name,
            edition,
            source_file,
            out_dir,
            is_test,
            ..
        } => {
            assert_eq!(crate_name, "cargo_instrument");
            assert_eq!(edition.as_deref(), Some("2021"));
            assert_eq!(source_file, &PathBuf::from("src/main.rs"));
            assert_eq!(out_dir, &Some(PathBuf::from("target/debug/deps")));
            assert!(!is_test);
            assert!(invocation.unit.is_eligible_for_analysis());
        }
        other => panic!("expected RustCrate, got {:?}", other),
    }
}

#[test]
fn test_discovery_empty_arguments_error() {
    use cargo_instrument::discovery::DiscoveryError;
    let result = CrateInvocation::parse(&[]);
    match result {
        Err(DiscoveryError::EmptyArguments) => {}
        other => panic!("expected EmptyArguments error, got {:?}", other),
    }
}

#[test]
fn test_discovery_passthrough_when_no_source_file() {
    let args = vec![
        "--crate-name".to_string(),
        "something".to_string(),
        "-C".to_string(),
        "opt-level=3".to_string(),
    ];
    let invocation = CrateInvocation::parse(&args).expect("parsing should succeed");
    assert!(matches!(
        invocation.unit,
        CompilationUnit::PassThrough { .. }
    ));
    assert!(!invocation.unit.is_eligible_for_analysis());
}

#[test]
fn test_discovery_has_opentelemetry_separated_and_equals() {
    let args1 = vec![
        "rustc".to_string(),
        "--crate-name".to_string(),
        "my_crate".to_string(),
        "src/lib.rs".to_string(),
        "--extern".to_string(),
        "opentelemetry=target/debug/deps/libopentelemetry.rlib".to_string(),
    ];
    let inv1 = CrateInvocation::parse(&args1).expect("parse args1");
    assert!(inv1.unit.has_opentelemetry());

    let args2 = vec![
        "rustc".to_string(),
        "--crate-name".to_string(),
        "my_crate".to_string(),
        "src/lib.rs".to_string(),
        "--extern=opentelemetry=target/debug/deps/libopentelemetry.rlib".to_string(),
    ];
    let inv2 = CrateInvocation::parse(&args2).expect("parse args2");
    assert!(inv2.unit.has_opentelemetry());

    let args3 = vec![
        "rustc".to_string(),
        "--crate-name".to_string(),
        "my_crate".to_string(),
        "src/lib.rs".to_string(),
        "--extern".to_string(),
        "noprelude:opentelemetry=target/debug/deps/libopentelemetry.rlib".to_string(),
    ];
    let inv3 = CrateInvocation::parse(&args3).expect("parse args3");
    assert!(inv3.unit.has_opentelemetry());

    let args4 = vec![
        "rustc".to_string(),
        "--crate-name".to_string(),
        "my_crate".to_string(),
        "src/lib.rs".to_string(),
        "--extern".to_string(),
        "serde=target/debug/deps/libserde.rlib".to_string(),
    ];
    let inv4 = CrateInvocation::parse(&args4).expect("parse args4");
    assert!(!inv4.unit.has_opentelemetry());
}

#[test]
fn test_discovery_has_otel_shim() {
    let args1 = vec![
        "rustc".to_string(),
        "--crate-name".to_string(),
        "app".to_string(),
        "src/main.rs".to_string(),
        "--extern".to_string(),
        "otel_shim=target/debug/deps/libotel_shim.rlib".to_string(),
    ];
    let inv1 = CrateInvocation::parse(&args1).expect("parse args1");
    assert!(inv1.unit.has_otel_shim());

    let args2 = vec![
        "rustc".to_string(),
        "--crate-name".to_string(),
        "app".to_string(),
        "src/main.rs".to_string(),
        "--extern=otel-shim=target/debug/deps/libotel_shim.rlib".to_string(),
    ];
    let inv2 = CrateInvocation::parse(&args2).expect("parse args2");
    assert!(inv2.unit.has_otel_shim());

    let args3 = vec![
        "rustc".to_string(),
        "--crate-name".to_string(),
        "app".to_string(),
        "src/main.rs".to_string(),
        "--extern".to_string(),
        "serde=target/debug/deps/libserde.rlib".to_string(),
    ];
    let inv3 = CrateInvocation::parse(&args3).expect("parse args3");
    assert!(!inv3.unit.has_otel_shim());
}

#[test]
fn test_crate_role_classification() {
    use cargo_instrument::discovery::CrateRole;
    use std::path::Path;

    #[cfg(windows)]
    let (root_path, other_path, reg_path, reg_otel_path) = (
        r"C:\workspace\my_project",
        r"C:\other\dep_a\src\lib.rs",
        r"C:\Users\user\.cargo\registry\src\index.crates.io-6f17d22bba15001f\serde-1.0.219\src\lib.rs",
        r"C:\Users\user\.cargo\registry\src\index.crates.io-6f17d22bba15001f\opentelemetry_sdk-0.32.0\src\lib.rs",
    );
    #[cfg(not(windows))]
    let (root_path, other_path, reg_path, reg_otel_path) = (
        "/workspace/my_project",
        "/other/dep_a/src/lib.rs",
        "/home/user/.cargo/registry/src/index.crates.io-6f17d22bba15001f/serde-1.0.219/src/lib.rs",
        "/home/user/.cargo/registry/src/index.crates.io-6f17d22bba15001f/opentelemetry_sdk-0.32.0/src/lib.rs",
    );

    let root = Path::new(root_path);

    // 1. Direct opentelemetry dependency -> Application
    let app_args = vec![
        "--crate-name".to_string(),
        "app".to_string(),
        "src/main.rs".to_string(),
        "--extern".to_string(),
        "opentelemetry=target/debug/deps/libopentelemetry.rlib".to_string(),
    ];
    let app_inv = CrateInvocation::parse(&app_args).unwrap();
    assert_eq!(app_inv.unit.role(root), CrateRole::Application);

    // 2. Local path dependency outside workspace
    let path_dep_args = vec![
        "--crate-name".to_string(),
        "dep_a".to_string(),
        other_path.to_string(),
    ];
    let path_dep_inv = CrateInvocation::parse(&path_dep_args).unwrap();
    assert_eq!(path_dep_inv.unit.role(root), CrateRole::LocalPathDependency);

    // 3. Workspace member dependency inside workspace root
    let ws_dep_args = vec![
        "--crate-name".to_string(),
        "dep_b".to_string(),
        "crates/dep_b/src/lib.rs".to_string(),
    ];
    let ws_dep_inv = CrateInvocation::parse(&ws_dep_args).unwrap();
    assert_eq!(
        ws_dep_inv.unit.role(root),
        CrateRole::WorkspaceMemberDependency
    );

    // 4. Registry dependency
    let reg_dep_args = vec![
        "--crate-name".to_string(),
        "serde".to_string(),
        reg_path.to_string(),
    ];
    let reg_dep_inv = CrateInvocation::parse(&reg_dep_args).unwrap();
    assert_eq!(reg_dep_inv.unit.role(root), CrateRole::RegistryDependency);

    // 5. Registry dependency that itself depends on opentelemetry (e.g. opentelemetry_sdk) -> RegistryDependency
    let reg_otel_args = vec![
        "--crate-name".to_string(),
        "opentelemetry_sdk".to_string(),
        reg_otel_path.to_string(),
        "--extern".to_string(),
        "opentelemetry=target/debug/deps/libopentelemetry.rlib".to_string(),
    ];
    let reg_otel_inv = CrateInvocation::parse(&reg_otel_args).unwrap();
    assert_eq!(reg_otel_inv.unit.role(root), CrateRole::RegistryDependency);

    // 6. Runtime shim self (otel_shim) -> RegistryDependency (never Application)
    let shim_args = vec![
        "--crate-name".to_string(),
        "otel_shim".to_string(),
        "otel-shim/src/lib.rs".to_string(),
        "--extern".to_string(),
        "opentelemetry=target/debug/deps/libopentelemetry.rlib".to_string(),
    ];
    let shim_inv = CrateInvocation::parse(&shim_args).unwrap();
    assert_eq!(shim_inv.unit.role(root), CrateRole::RegistryDependency);
}
