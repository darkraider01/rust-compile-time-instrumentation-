pub mod ast;
pub mod candidate;
pub mod discovery;
pub mod session;
pub mod transform;
pub mod unit;
pub mod wrapper;

pub use ast::{analyze_source_file, analyze_source_str, check_application_preflight, AstError};
pub use candidate::{Candidate, DiscoveryReport, FunctionKind, UnsafePolicy};
pub use discovery::{CompilationUnit, CrateInvocation, CrateRole, DiscoveryError};
pub use session::{SessionPlan, SkipCause, SESSION_ENV};
pub use transform::{
    detect_line_ending, paths_are_identical, transform_source_file, transform_source_file_scoped,
    transform_source_file_scoped_with_emitter, transform_source_str,
    transform_source_str_with_emitter, transform_source_str_with_native_otel,
    transform_source_str_with_trampoline, ByteEdit, Emitter, NativeOtelEmitter, SentinelEmitter,
    SkipReason, SkippedCandidate, SpanKind, TrampolineEmitter, TransformError, TransformationPlan,
    INSTRUMENT_ANCHOR_PREFIX,
};
pub use unit::UnitId;
pub use wrapper::{run_wrapper, WrapperConfig, WrapperError, DEBUG_ENV, RECURSION_GUARD_ENV};
