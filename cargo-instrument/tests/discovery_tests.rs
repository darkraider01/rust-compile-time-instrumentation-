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
