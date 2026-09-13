// HISTORICAL SPIKE — NOT PRODUCTION TOOLING.
//
// ADR-011/P2.4 evidence for `--extern` artifact-ordering investigation. This
// standalone wrapper is not built by the workspace and does not establish a
// supported dependency-instrumentation implementation.
//
// Discovers the rlib at the moment dep_lib compiles, as the real tool would have to.
use std::env;
use std::fs;
use std::process::{Command, exit};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let (rustc, rest) = args.split_first().expect("rustc path");

    let val = |flag: &str| rest.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone());
    let crate_name = val("--crate-name").unwrap_or_default();

    let mut cmd = Command::new(rustc);
    cmd.args(rest);

    if crate_name == "dep_lib" {
        // Cargo passes -L dependency=<...>/deps; that is where rlibs land.
        let deps_dir = rest
            .windows(2)
            .find(|w| w[0] == "-L" && w[1].starts_with("dependency="))
            .map(|w| w[1].trim_start_matches("dependency=").to_string())
            .unwrap_or_default();

        let found: Vec<String> = fs::read_dir(&deps_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.starts_with("libopentelemetry-") && n.ends_with(".rlib"))
                    .collect()
            })
            .unwrap_or_default();

        eprintln!("[probe] compiling dep_lib; deps dir = {deps_dir}");
        eprintln!("[probe] opentelemetry rlibs visible RIGHT NOW: {found:?}");

        match found.len() {
            1 => {
                let p = format!("{deps_dir}/{}", found[0]);
                eprintln!("[probe] INJECTING {p}");
                cmd.arg("--extern").arg(format!("opentelemetry={p}"));
            }
            0 => eprintln!("[probe] *** NOT BUILT YET - cannot inject ***"),
            n => eprintln!("[probe] *** AMBIGUOUS: {n} candidates ***"),
        }
    }

    exit(cmd.status().expect("spawn rustc").code().unwrap_or(1));
}
