use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::candidate::Candidate;

/// Hard errors that prevent processing an entire file.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransformError {
    #[error("Source file '{path}' cannot be modified in-place")]
    InPlaceModificationDisallowed { path: PathBuf },

    #[error("Offset {offset} in edit {range:?} does not fall on a valid UTF-8 character boundary")]
    InvalidUtf8Boundary { offset: usize, range: Range<usize> },

    #[error("Edit range {range:?} is out of bounds for source buffer of length {source_len}")]
    OutOfBounds {
        range: Range<usize>,
        source_len: usize,
    },

    #[error("IO error on '{path}': {message}")]
    Io { path: PathBuf, message: String },
}

/// Structured reasons why an individual candidate was safely skipped during planning.
///
/// Implements S11 fail-open at candidate granularity: a malformed individual candidate
/// is skipped and logged, rather than aborting transformation of the entire file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    OutOfBounds {
        range: Range<usize>,
        source_len: usize,
    },
    InvalidUtf8Boundary {
        offset: usize,
    },
    MissingOpeningBrace {
        offset: usize,
        found: u8,
    },
    OverlappingWithPrevious {
        previous: Range<usize>,
        current: Range<usize>,
    },
    AlreadyInstrumented,
    AsyncDeferred,
}

/// Diagnostic record of an individual candidate that was skipped during transformation planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedCandidate {
    pub function_name: String,
    pub candidate_range: Range<usize>,
    pub reason: SkipReason,
}

/// Marker comment prefix used to identify instrumented sites.
pub const INSTRUMENT_ANCHOR_PREFIX: &str = "/* __cargo_instrument_anchor:";

/// Pluggable code emission strategy per ADR-006 ("the emitter is a seam").
///
/// Decouples edit planning, candidate validation, and byte splicing from
/// replacement text generation. P1.4 provides `SentinelEmitter`; P1.5 swaps this
/// with a native OpenTelemetry emitter without modifying the splicing engine.
pub trait Emitter: Send + Sync {
    /// Generate replacement text to inject immediately inside the opening brace `{` of `candidate`.
    ///
    /// `line_ending` indicates the detected line ending of the source file (`"\n"` or `"\r\n"`).
    fn emit_body_prefix(&self, candidate: &Candidate, line_ending: &str) -> String;

    /// Generate replacement text to inject immediately before the closing brace `}` of `candidate`.
    /// Default implementation returns an empty string (pure prefix/RAII emission).
    fn emit_body_suffix(&self, _candidate: &Candidate, _line_ending: &str) -> String {
        String::new()
    }

    /// Whether this emitter supports instrumenting asynchronous functions.
    /// Defaults to `true` so general emitters (e.g. `SentinelEmitter`) are unaffected.
    /// `NativeOtelEmitter` returns `true` as implemented in Milestone P1.6.
    fn handles_async(&self) -> bool {
        true
    }
}

/// Default minimal sentinel emitter for Milestone P1.4.
///
/// Emits a zero-dependency sentinel to prove surgical byte-splicing mechanics
/// without coupling to runtime OpenTelemetry crates or introducing fake lifecycle semantics.
#[derive(Debug, Clone, Default)]
pub struct SentinelEmitter;

impl Emitter for SentinelEmitter {
    fn emit_body_prefix(&self, candidate: &Candidate, line_ending: &str) -> String {
        format!(
            "{nl}    /* __cargo_instrument_anchor: \"{name}\" */{nl}    let _cargo_instrument_sentinel = ();",
            nl = line_ending,
            name = candidate.function_name
        )
    }
}

/// Detect the line-ending convention used in the source text.
/// Returns `"\r\n"` if CRLF is dominant, otherwise `"\n"`.
pub fn detect_line_ending(source: &str) -> &'static str {
    let sample = if source.len() > 4096 {
        &source[..4096]
    } else {
        source
    };
    let crlf_count = sample.matches("\r\n").count();
    let lf_count = sample.matches('\n').count().saturating_sub(crlf_count);
    if crlf_count > lf_count {
        "\r\n"
    } else {
        "\n"
    }
}

/// A single discrete textual edit to be applied to the original source text.
///
/// CRITICAL INVARIANT:
/// `start` and `end` refer strictly to byte offsets in the ORIGINAL, IMMUTABLE source buffer.
/// They are never shifted or recalculated relative to intermediate spliced states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteEdit {
    /// Start byte offset in the original immutable source buffer.
    pub start: usize,
    /// End byte offset in the original immutable source buffer (for pure insertion, `start == end`).
    pub end: usize,
    /// Replacement text to splice in place of `source[start..end]`.
    pub replacement: String,
}

/// A validated, sorted, non-overlapping transformation plan for a single source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformationPlan {
    /// Ordered list of discrete byte edits, sorted by `start` offset ascending.
    pub edits: Vec<ByteEdit>,
    /// Candidates that were skipped during planning with structured reasons (S11 fail-open).
    pub skipped: Vec<SkippedCandidate>,
}

impl TransformationPlan {
    /// Build and validate a transformation plan from original source text and candidates
    /// using the default `SentinelEmitter`.
    pub fn build(source: &str, candidates: &[Candidate]) -> Result<Self, TransformError> {
        Self::build_with_emitter(source, candidates, &SentinelEmitter)
    }

    /// Build and validate a transformation plan using the `NativeOtelEmitter` for a given crate.
    pub fn build_with_native_otel(
        source: &str,
        crate_name: &str,
        candidates: &[Candidate],
    ) -> Result<Self, TransformError> {
        let emitter = NativeOtelEmitter::new(crate_name);
        Self::build_with_emitter(source, candidates, &emitter)
    }

    /// Build and validate a transformation plan using a pluggable `Emitter` (ADR-006).
    ///
    /// Architectural guarantees:
    /// 1. Every edit offset references the original, immutable `source` buffer.
    /// 2. Candidate ordering is normalized deterministically (sorted by `body_byte_range.start`,
    ///    tie-broken by `body_byte_range.end` and `function_name`).
    /// 3. H2 Fail-Open: Malformed individual candidates (out of bounds, invalid UTF-8 boundary,
    ///    missing opening brace, overlapping) are skipped and recorded in `plan.skipped`,
    ///    allowing healthy candidates in the same file to be safely transformed.
    /// 4. M1: Uses the detected line ending (`\r\n` vs `\n`) for all emitted text.
    /// 5. M3: Uses structurally constrained idempotence detection (anchor must follow body `{`).
    pub fn build_with_emitter<E: Emitter + ?Sized>(
        source: &str,
        candidates: &[Candidate],
        emitter: &E,
    ) -> Result<Self, TransformError> {
        let source_len = source.len();
        let line_ending = detect_line_ending(source);

        // 1. Sort candidates deterministically
        let mut sorted_candidates: Vec<&Candidate> = candidates.iter().collect();
        sorted_candidates.sort_by(|a, b| {
            a.body_byte_range
                .start
                .cmp(&b.body_byte_range.start)
                .then_with(|| a.body_byte_range.end.cmp(&b.body_byte_range.end))
                .then_with(|| a.function_name.cmp(&b.function_name))
        });

        let mut edits = Vec::new();
        let mut skipped = Vec::new();
        let mut last_valid_range: Option<Range<usize>> = None;

        for c in sorted_candidates {
            let r = &c.body_byte_range;

            // Check 0: Async capability check (ADR-006 / P1.5 scope boundary)
            if c.is_async && !emitter.handles_async() {
                skipped.push(SkippedCandidate {
                    function_name: c.function_name.clone(),
                    candidate_range: r.clone(),
                    reason: SkipReason::AsyncDeferred,
                });
                continue;
            }

            // Check 1: Candidate bounds in buffer
            if r.start >= r.end || r.end > source_len {
                skipped.push(SkippedCandidate {
                    function_name: c.function_name.clone(),
                    candidate_range: r.clone(),
                    reason: SkipReason::OutOfBounds {
                        range: r.clone(),
                        source_len,
                    },
                });
                continue;
            }

            // Check 2: UTF-8 character boundaries
            if !source.is_char_boundary(r.start) {
                skipped.push(SkippedCandidate {
                    function_name: c.function_name.clone(),
                    candidate_range: r.clone(),
                    reason: SkipReason::InvalidUtf8Boundary { offset: r.start },
                });
                continue;
            }
            if !source.is_char_boundary(r.end) {
                skipped.push(SkippedCandidate {
                    function_name: c.function_name.clone(),
                    candidate_range: r.clone(),
                    reason: SkipReason::InvalidUtf8Boundary { offset: r.end },
                });
                continue;
            }

            // Check 3: Overlap with previously accepted candidate
            if let Some(ref prev) = last_valid_range {
                if r.start < prev.end {
                    skipped.push(SkippedCandidate {
                        function_name: c.function_name.clone(),
                        candidate_range: r.clone(),
                        reason: SkipReason::OverlappingWithPrevious {
                            previous: prev.clone(),
                            current: r.clone(),
                        },
                    });
                    continue;
                }
            }

            // Check 4: Opening brace check
            let first_byte = source.as_bytes()[r.start];
            if first_byte != b'{' {
                skipped.push(SkippedCandidate {
                    function_name: c.function_name.clone(),
                    candidate_range: r.clone(),
                    reason: SkipReason::MissingOpeningBrace {
                        offset: r.start,
                        found: first_byte,
                    },
                });
                continue;
            }

            // Check 5: Structurally constrained idempotence detection (M3)
            if body_starts_with_anchor_sentinel(source, r.start) {
                skipped.push(SkippedCandidate {
                    function_name: c.function_name.clone(),
                    candidate_range: r.clone(),
                    reason: SkipReason::AlreadyInstrumented,
                });
                continue;
            }

            // Check 6: Insertion point character boundary
            let insert_offset = r.start + 1;
            if !source.is_char_boundary(insert_offset) {
                skipped.push(SkippedCandidate {
                    function_name: c.function_name.clone(),
                    candidate_range: r.clone(),
                    reason: SkipReason::InvalidUtf8Boundary {
                        offset: insert_offset,
                    },
                });
                continue;
            }

            // 7. Emit replacement using pluggable emitter and detected line ending
            let prefix = emitter.emit_body_prefix(c, line_ending);
            let suffix = emitter.emit_body_suffix(c, line_ending);

            if !suffix.is_empty() {
                let suffix_offset = r.end.saturating_sub(1);
                if suffix_offset <= insert_offset || !source.is_char_boundary(suffix_offset) {
                    skipped.push(SkippedCandidate {
                        function_name: c.function_name.clone(),
                        candidate_range: r.clone(),
                        reason: SkipReason::InvalidUtf8Boundary {
                            offset: suffix_offset,
                        },
                    });
                    continue;
                }
                let last_byte = source.as_bytes()[suffix_offset];
                if last_byte != b'}' {
                    skipped.push(SkippedCandidate {
                        function_name: c.function_name.clone(),
                        candidate_range: r.clone(),
                        reason: SkipReason::MissingOpeningBrace {
                            offset: suffix_offset,
                            found: last_byte,
                        },
                    });
                    continue;
                }

                edits.push(ByteEdit {
                    start: insert_offset,
                    end: insert_offset,
                    replacement: prefix,
                });
                edits.push(ByteEdit {
                    start: suffix_offset,
                    end: suffix_offset,
                    replacement: suffix,
                });
            } else {
                edits.push(ByteEdit {
                    start: insert_offset,
                    end: insert_offset,
                    replacement: prefix,
                });
            }

            last_valid_range = Some(r.clone());
        }

        Ok(TransformationPlan { edits, skipped })
    }

    /// Apply planned edits to the original source buffer in a single pass.
    ///
    /// Constructs a brand new `String` buffer by slicing unchanged byte ranges
    /// from the original source buffer and appending replacements:
    /// `source[0..edit[0].start]` + `replacement[0]` + `source[edit[0].end..edit[1].start]` ...
    ///
    /// Guaranteed:
    /// - The original source buffer is never mutated.
    /// - Comments and formatting outside transformed regions are bit-for-bit identical.
    /// - Earlier replacements never shift or invalidate subsequent edits because all offsets
    ///   are indexed against the immutable original `source` buffer.
    pub fn apply(&self, source: &str) -> Result<String, TransformError> {
        let total_extra_capacity: usize = self.edits.iter().map(|e| e.replacement.len()).sum();
        let mut result = String::with_capacity(source.len() + total_extra_capacity);
        let mut last_offset = 0;

        for edit in &self.edits {
            if edit.start > source.len() || edit.end > source.len() || edit.start > edit.end {
                return Err(TransformError::OutOfBounds {
                    range: edit.start..edit.end,
                    source_len: source.len(),
                });
            }

            if !source.is_char_boundary(edit.start) {
                return Err(TransformError::InvalidUtf8Boundary {
                    offset: edit.start,
                    range: edit.start..edit.end,
                });
            }
            if !source.is_char_boundary(edit.end) {
                return Err(TransformError::InvalidUtf8Boundary {
                    offset: edit.end,
                    range: edit.start..edit.end,
                });
            }

            // Copy untouched original bytes between last_offset and edit.start
            result.push_str(&source[last_offset..edit.start]);
            // Insert replacement
            result.push_str(&edit.replacement);
            last_offset = edit.end;
        }

        // Copy remaining untouched original bytes
        result.push_str(&source[last_offset..]);
        Ok(result)
    }
}

/// Structurally constrained idempotence detector (M3).
///
/// Verifies that the candidate body structurally begins with the anchor comment
/// directly following the opening `{`, rather than matching raw substrings anywhere
/// inside string literals or inner statements.
///
/// Verifies the presence of the anchor comment block `/* __cargo_instrument_anchor: ... */`
/// at the head of the function body without coupling to a specific sentinel statement,
/// preserving forward-compatibility with pluggable emitters (H3 / ADR-006).
fn body_starts_with_anchor_sentinel(source: &str, body_start: usize) -> bool {
    if body_start + 1 >= source.len() {
        return false;
    }
    let inner = &source[body_start + 1..];
    let trimmed = inner.trim_start();
    if !trimmed.starts_with(INSTRUMENT_ANCHOR_PREFIX) {
        return false;
    }
    if let Some(rest) = trimmed.strip_prefix(INSTRUMENT_ANCHOR_PREFIX) {
        return rest.contains("*/");
    }
    false
}

/// Convenience helper: create plan and transform source text in-memory.
pub fn transform_source_str(
    source: &str,
    candidates: &[Candidate],
) -> Result<String, TransformError> {
    let plan = TransformationPlan::build(source, candidates)?;
    plan.apply(source)
}

/// Convenience helper: transform source text using a custom `Emitter`.
pub fn transform_source_str_with_emitter<E: Emitter + ?Sized>(
    source: &str,
    candidates: &[Candidate],
    emitter: &E,
) -> Result<String, TransformError> {
    let plan = TransformationPlan::build_with_emitter(source, candidates, emitter)?;
    plan.apply(source)
}

/// Convenience helper: create plan and transform source text using native OpenTelemetry emitter.
pub fn transform_source_str_with_native_otel(
    source: &str,
    crate_name: &str,
    candidates: &[Candidate],
) -> Result<String, TransformError> {
    let emitter = NativeOtelEmitter::new(crate_name);
    let plan = TransformationPlan::build_with_emitter(source, candidates, &emitter)?;
    plan.apply(source)
}

/// Transform a source file from disk using candidates that have already been scoped to this file.
///
/// NON-NEGOTIABLE INVARIANT:
/// The input source file must NEVER be modified in-place. If `input_path` and `output_path`
/// resolve to the same file, this function immediately returns an error.
pub fn transform_source_file_scoped(
    input_path: &Path,
    output_path: &Path,
    scoped_candidates: &[Candidate],
) -> Result<TransformationPlan, TransformError> {
    transform_source_file_scoped_with_emitter(
        input_path,
        output_path,
        scoped_candidates,
        &SentinelEmitter,
    )
}

/// Transform a source file from disk using a custom emitter and scoped candidates.
///
/// NON-NEGOTIABLE INVARIANT:
/// The input source file must NEVER be modified in-place. If `input_path` and `output_path`
/// resolve to the same file, this function immediately returns an error.
pub fn transform_source_file_scoped_with_emitter<E: Emitter + ?Sized>(
    input_path: &Path,
    output_path: &Path,
    scoped_candidates: &[Candidate],
    emitter: &E,
) -> Result<TransformationPlan, TransformError> {
    // 1. Guard against in-place modification
    if paths_are_identical(input_path, output_path) {
        return Err(TransformError::InPlaceModificationDisallowed {
            path: input_path.to_path_buf(),
        });
    }

    // 2. Read original source bytes
    let source_bytes = fs::read(input_path).map_err(|e| TransformError::Io {
        path: input_path.to_path_buf(),
        message: e.to_string(),
    })?;

    let source_text = String::from_utf8(source_bytes).map_err(|e| TransformError::Io {
        path: input_path.to_path_buf(),
        message: format!("Source is not valid UTF-8: {e}"),
    })?;

    // 3. Perform transformation
    let plan = TransformationPlan::build_with_emitter(&source_text, scoped_candidates, emitter)?;
    let transformed = plan.apply(&source_text)?;

    // 4. Ensure parent directories exist for output path
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| TransformError::Io {
                path: parent.to_path_buf(),
                message: e.to_string(),
            })?;
        }
    }

    // 5. Write transformed output to destination (preserve mtime if content is identical)
    if output_path.exists() {
        if let Ok(existing_bytes) = fs::read(output_path) {
            if existing_bytes == transformed.as_bytes() {
                return Ok(plan);
            }
        }
    }

    // Write to a temporary file in the same directory and atomically rename into place
    static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp_output = output_path.with_extension(format!("tmp.{}.{}", std::process::id(), counter));
    fs::write(&temp_output, transformed.as_bytes()).map_err(|e| TransformError::Io {
        path: temp_output.to_path_buf(),
        message: e.to_string(),
    })?;

    // Synchronize output modification time with original source file so Cargo's
    // build fingerprint does not consider the file modified during/after the build.
    if let Ok(metadata) = fs::metadata(input_path) {
        if let Ok(mtime) = metadata.modified() {
            if let Ok(file) = fs::OpenOptions::new().write(true).open(&temp_output) {
                let times = fs::FileTimes::new().set_modified(mtime);
                let _ = file.set_times(times);
            }
        }
    }

    if let Err(e) = fs::rename(&temp_output, output_path) {
        let _ = fs::remove_file(&temp_output);
        fs::write(output_path, transformed.as_bytes()).map_err(|write_err| TransformError::Io {
            path: output_path.to_path_buf(),
            message: format!("rename failed ({e}); direct write failed ({write_err})"),
        })?;
        if let Ok(metadata) = fs::metadata(input_path) {
            if let Ok(mtime) = metadata.modified() {
                if let Ok(file) = fs::OpenOptions::new().write(true).open(output_path) {
                    let times = fs::FileTimes::new().set_modified(mtime);
                    let _ = file.set_times(times);
                }
            }
        }
    }

    Ok(plan)
}

/// Transform a source file, matching candidates by exact canonical or normalized path equality.
///
/// C1 INVARIANT: Never matches candidates by basename or `file_name()` alone.
pub fn transform_source_file(
    input_path: &Path,
    output_path: &Path,
    candidates: &[Candidate],
) -> Result<TransformationPlan, TransformError> {
    let scoped: Vec<Candidate> = candidates
        .iter()
        .filter(|c| paths_are_identical(&c.source_file, input_path))
        .cloned()
        .collect();

    transform_source_file_scoped(input_path, output_path, &scoped)
}

/// Check if two paths resolve to the exact same file.
///
/// Handles canonicalization, relative components, and platform path separators.
pub fn paths_are_identical(p1: &Path, p2: &Path) -> bool {
    if p1 == p2 {
        return true;
    }
    match (p1.canonicalize(), p2.canonicalize()) {
        (Ok(c1), Ok(c2)) => c1 == c2,
        _ => normalize_path(p1) == normalize_path(p2),
    }
}

/// Normalize path components (removing `.` and resolving `..`).
pub fn normalize_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(c.as_os_str()),
        }
    }
    out
}

/// OpenTelemetry span kind for compile-time generated spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanKind {
    /// Internal span representing in-process execution (default for P1.5).
    #[default]
    Internal,
}

/// Native OpenTelemetry emitter generating zero-dependency-on-tracing instrumentation code (P1.5).
///
/// Emits pure synchronous OpenTelemetry 0.32.0 API calls with fully-qualified trait methods
/// to ensure clean compilation under both `rustc -D warnings` and `cargo clippy -D warnings`.
///
/// ### Documented Runtime Cost & Deferred Optimization (M2 / §16.3)
/// Tracer acquisition (`let __otel_tracer = opentelemetry::global::tracer("{crate_name}");`)
/// is currently performed at the entry of each instrumented function body. Per invocation, this
/// acquires a global `RwLock` read lock, clones the `Arc<TracerProvider>`, and allocates a
/// `BoxedTracer` on the heap. While optimal for crate boundary isolation without global static
/// boilerplate, pre-caching the tracer in a crate-level `static` / `OnceLock` is formally
/// deferred per the §16.3 precedent ("Not Phase 1 — measure first").
#[derive(Debug, Clone)]
pub struct NativeOtelEmitter {
    /// The crate name used as the OpenTelemetry tracer name (instrumentation scope).
    pub crate_name: String,
}

impl NativeOtelEmitter {
    /// Create a new native OpenTelemetry emitter with the given crate name.
    pub fn new(crate_name: impl Into<String>) -> Self {
        Self {
            crate_name: crate_name.into(),
        }
    }
}

impl Emitter for NativeOtelEmitter {
    fn emit_body_prefix(&self, candidate: &Candidate, line_ending: &str) -> String {
        let name = &candidate.function_name;
        let crate_name = &self.crate_name;
        let nl = line_ending;

        if candidate.is_async {
            // P1.6: Async functions wrap the body in `FutureExt::with_context(async move { ... })`.
            // Async generators share lifetime bounds with the outer future, so `Result<&mut T, E>`
            // compiles cleanly without closure-escape issues. Thus, `returns_reference_or_lifetime`
            // is strictly exclusive to the sync path; async branches test `returns_result` only.
            if candidate.returns_result {
                format!(
                    "{nl}    /* __cargo_instrument_anchor: \"{name}\" */\
                     {nl}    let __otel_tracer = opentelemetry::global::tracer(\"{crate_name}\");\
                     {nl}    let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, \"{name}\")\
                     {nl}        .with_kind(opentelemetry::trace::SpanKind::Internal)\
                     {nl}        .start(&__otel_tracer);\
                     {nl}    let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);\
                     {nl}    let __otel_res: Result<_, _> = opentelemetry::trace::FutureExt::with_context(async move {{"
                )
            } else {
                format!(
                    "{nl}    /* __cargo_instrument_anchor: \"{name}\" */\
                     {nl}    let __otel_tracer = opentelemetry::global::tracer(\"{crate_name}\");\
                     {nl}    let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, \"{name}\")\
                     {nl}        .with_kind(opentelemetry::trace::SpanKind::Internal)\
                     {nl}        .start(&__otel_tracer);\
                     {nl}    let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);\
                     {nl}    opentelemetry::trace::FutureExt::with_context(async move {{"
                )
            }
        } else {
            // C1 & Aliased &mut: Synchronous functions returning references or carrying lifetimes
            // (&mut T, Result<&mut T, E>, Result<MutName<'_>, E>) cannot be wrapped in
            // closures (captured variable cannot escape FnMut closure body).
            // They fall back to prefix-only instrumentation per §16.10.
            if candidate.returns_result && !candidate.returns_reference_or_lifetime {
                format!(
                    "{nl}    /* __cargo_instrument_anchor: \"{name}\" */\
                     {nl}    let __otel_tracer = opentelemetry::global::tracer(\"{crate_name}\");\
                     {nl}    let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, \"{name}\")\
                     {nl}        .with_kind(opentelemetry::trace::SpanKind::Internal)\
                     {nl}        .start(&__otel_tracer);\
                     {nl}    let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);\
                     {nl}    let __otel_guard = __otel_cx.clone().attach();\
                     {nl}    #[allow(clippy::redundant_closure_call)]\
                     {nl}    let __otel_res: Result<_, _> = (|| {{"
                )
            } else {
                format!(
                    "{nl}    /* __cargo_instrument_anchor: \"{name}\" */\
                     {nl}    let __otel_tracer = opentelemetry::global::tracer(\"{crate_name}\");\
                     {nl}    let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, \"{name}\")\
                     {nl}        .with_kind(opentelemetry::trace::SpanKind::Internal)\
                     {nl}        .start(&__otel_tracer);\
                     {nl}    let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);\
                     {nl}    let __otel_guard = __otel_cx.attach();"
                )
            }
        }
    }

    fn emit_body_suffix(&self, candidate: &Candidate, line_ending: &str) -> String {
        let nl = line_ending;
        if candidate.is_async {
            if candidate.returns_result {
                format!(
                    "{nl}    }}, __otel_cx.clone()).await;\
                     {nl}    if __otel_res.is_err() {{\
                     {nl}        opentelemetry::trace::TraceContextExt::span(&__otel_cx)\
                     {nl}            .set_status(opentelemetry::trace::Status::error(\"\"));\
                     {nl}    }}\
                     {nl}    __otel_res{nl}"
                )
            } else {
                format!("{nl}    }}, __otel_cx).await{nl}")
            }
        } else if candidate.returns_result && !candidate.returns_reference_or_lifetime {
            format!(
                "{nl}    }})();\
                 {nl}    if __otel_res.is_err() {{\
                 {nl}        opentelemetry::trace::TraceContextExt::span(&__otel_cx)\
                 {nl}            .set_status(opentelemetry::trace::Status::error(\"\"));\
                 {nl}    }}\
                 {nl}    __otel_res{nl}"
            )
        } else {
            String::new()
        }
    }

    fn handles_async(&self) -> bool {
        true
    }
}

/// Tier-2 OpenTelemetry trampoline emitter for dependency crates (Milestone P1.7).
///
/// Splices calls to `extern "C"` trampoline functions (`__otel_span_enter`, `__otel_span_exit`,
/// and conditionally `__otel_span_set_error`) without introducing any Cargo dependencies into
/// the target crate's `Cargo.toml`.
///
/// Features:
/// - M1: Block-scoped minimal symbol declarations per call site (2 symbols for non-Result, 3 for Result).
/// - Edition-aware: Emits `unsafe extern "C"` in 2024 edition, `extern "C"` in earlier editions.
/// - G3: Emits `#[allow(unsafe_code)]` when `unsafe_policy == UnsafePolicy::Denied` to recover ecosystem coverage.
/// - RAII guard: Implements `Drop` to guarantee LIFO `__otel_span_exit` on normal return or unwind.
/// - Result handling: Wraps function body in a closure to inspect status without modifying return values,
///   calling `__otel_span_set_error(handle)` on `Err`.
/// - Async deferred: `handles_async() -> false`, skipping async candidates per §12.3 / §16.3 / FE-13.
#[derive(Debug, Clone)]
pub struct TrampolineEmitter {
    pub crate_name: String,
    pub edition: Option<String>,
    pub unsafe_policy: crate::candidate::UnsafePolicy,
}

impl TrampolineEmitter {
    pub fn new(
        crate_name: impl Into<String>,
        edition: Option<String>,
        unsafe_policy: crate::candidate::UnsafePolicy,
    ) -> Self {
        Self {
            crate_name: crate_name.into(),
            edition,
            unsafe_policy,
        }
    }
}

impl Emitter for TrampolineEmitter {
    fn emit_body_prefix(&self, candidate: &Candidate, line_ending: &str) -> String {
        let name = &candidate.function_name;
        let nl = line_ending;
        let is_2024 = self.edition.as_deref() == Some("2024");
        let extern_kw = if is_2024 {
            "unsafe extern \"C\""
        } else {
            "extern \"C\""
        };

        let allow_unsafe = if self.unsafe_policy == crate::candidate::UnsafePolicy::Denied {
            format!("{nl}    #[allow(unsafe_code)]")
        } else {
            String::new()
        };

        let allow_unsafe_in_drop = if self.unsafe_policy == crate::candidate::UnsafePolicy::Denied {
            format!("{nl}                #[allow(unsafe_code)]")
        } else {
            String::new()
        };

        if candidate.returns_result && !candidate.returns_reference_or_lifetime {
            // Sync Result: 3 symbols (enter, exit, set_error) + closure wrapper
            format!(
                "{nl}    /* __cargo_instrument_anchor: \"{name}\" */\
                 {allow_unsafe}\
                 {nl}    {extern_kw} {{\
                 {nl}        fn __otel_span_enter(\
                 {nl}            name: *const u8,\
                 {nl}            name_len: usize,\
                 {nl}            file: *const u8,\
                 {nl}            file_len: usize,\
                 {nl}            line: u32,\
                 {nl}            kind: u8,\
                 {nl}        ) -> u64;\
                 {nl}        fn __otel_span_exit(handle: u64);\
                 {nl}        fn __otel_span_set_error(handle: u64);\
                 {nl}    }}\
                 {nl}    struct __OtelGuard(u64);\
                 {nl}    impl Drop for __OtelGuard {{\
                 {nl}        fn drop(&mut self) {{\
                 {nl}            if self.0 != 0 {{\
                 {allow_unsafe_in_drop}\
                 {nl}                unsafe {{\
                 {nl}                    __otel_span_exit(self.0);\
                 {nl}                }}\
                 {nl}            }}\
                 {nl}        }}\
                 {nl}    }}\
                 {nl}    let __otel_name = \"{name}\";\
                 {nl}    let __otel_file = file!();\
                 {allow_unsafe}\
                 {nl}    let __otel_guard = __OtelGuard(unsafe {{\
                 {nl}        __otel_span_enter(\
                 {nl}            __otel_name.as_ptr(),\
                 {nl}            __otel_name.len(),\
                 {nl}            __otel_file.as_ptr(),\
                 {nl}            __otel_file.len(),\
                 {nl}            line!(),\
                 {nl}            0u8,\
                 {nl}        )\
                 {nl}    }});\
                 {nl}    #[allow(clippy::redundant_closure_call)]\
                 {nl}    let __otel_res: core::result::Result<_, _> = (|| {{"
            )
        } else {
            // Sync non-Result / returns_reference_or_lifetime: 2 symbols (enter, exit)
            format!(
                "{nl}    /* __cargo_instrument_anchor: \"{name}\" */\
                 {allow_unsafe}\
                 {nl}    {extern_kw} {{\
                 {nl}        fn __otel_span_enter(\
                 {nl}            name: *const u8,\
                 {nl}            name_len: usize,\
                 {nl}            file: *const u8,\
                 {nl}            file_len: usize,\
                 {nl}            line: u32,\
                 {nl}            kind: u8,\
                 {nl}        ) -> u64;\
                 {nl}        fn __otel_span_exit(handle: u64);\
                 {nl}    }}\
                 {nl}    struct __OtelGuard(u64);\
                 {nl}    impl Drop for __OtelGuard {{\
                 {nl}        fn drop(&mut self) {{\
                 {nl}            if self.0 != 0 {{\
                 {allow_unsafe_in_drop}\
                 {nl}                unsafe {{\
                 {nl}                    __otel_span_exit(self.0);\
                 {nl}                }}\
                 {nl}            }}\
                 {nl}        }}\
                 {nl}    }}\
                 {nl}    let __otel_name = \"{name}\";\
                 {nl}    let __otel_file = file!();\
                 {allow_unsafe}\
                 {nl}    let _otel_guard = __OtelGuard(unsafe {{\
                 {nl}        __otel_span_enter(\
                 {nl}            __otel_name.as_ptr(),\
                 {nl}            __otel_name.len(),\
                 {nl}            __otel_file.as_ptr(),\
                 {nl}            __otel_file.len(),\
                 {nl}            line!(),\
                 {nl}            0u8,\
                 {nl}        )\
                 {nl}    }});"
            )
        }
    }

    fn emit_body_suffix(&self, candidate: &Candidate, line_ending: &str) -> String {
        let nl = line_ending;
        let allow_unsafe = if self.unsafe_policy == crate::candidate::UnsafePolicy::Denied {
            format!("{nl}        #[allow(unsafe_code)]")
        } else {
            String::new()
        };

        if candidate.returns_result && !candidate.returns_reference_or_lifetime {
            format!(
                "{nl}    }})();\
                 {nl}    if __otel_res.is_err() && __otel_guard.0 != 0 {{\
                 {allow_unsafe}\
                 {nl}        unsafe {{\
                 {nl}            __otel_span_set_error(__otel_guard.0);\
                 {nl}        }}\
                 {nl}    }}\
                 {nl}    __otel_res{nl}"
            )
        } else {
            String::new()
        }
    }

    fn handles_async(&self) -> bool {
        false
    }
}

/// Convenience helper: create plan and transform source text using trampoline emitter.
pub fn transform_source_str_with_trampoline(
    source: &str,
    crate_name: &str,
    edition: Option<String>,
    unsafe_policy: crate::candidate::UnsafePolicy,
    candidates: &[Candidate],
) -> Result<String, TransformError> {
    let emitter = TrampolineEmitter::new(crate_name, edition, unsafe_policy);
    let plan = TransformationPlan::build_with_emitter(source, candidates, &emitter)?;
    plan.apply(source)
}
