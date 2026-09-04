pub mod ast;
pub mod candidate;
pub mod discovery;
pub mod wrapper;

pub use ast::{analyze_source_file, analyze_source_str, AstError};
pub use candidate::{Candidate, DiscoveryReport, FunctionKind, UnsafePolicy};
pub use discovery::{CompilationUnit, CrateInvocation, DiscoveryError};
pub use wrapper::{run_wrapper, WrapperConfig, WrapperError, DEBUG_ENV, RECURSION_GUARD_ENV};
