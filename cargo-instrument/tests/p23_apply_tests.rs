#![cfg_attr(not(windows), allow(dead_code))]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const MARKER: &str = "/* __cargo_instrument_rust:p23 */";

/// This is process-level because Cargo must own its rustfix `RUSTC_WRAPPER`
/// proxy while the custom compiler occupies `RUSTC`.
#[test]
#[ignore = "requires the pinned P2.3 nightly toolchain and rustc-dev"]
fn cargo_subcommand_apply_is_owned_idempotent_and_async_safe() {
    let fixture = Fixture::new();
    let original = fs::read_to_string(fixture.app_source()).unwrap();
    let dependency_before = fs::read_to_string(fixture.dependency_source()).unwrap();
    let nested_dependency_before = fs::read_to_string(fixture.nested_dependency_source()).unwrap();
    let outside_source_before = fs::read_to_string(fixture.outside_source()).unwrap();

    let first = fixture.run_apply();
    eprintln!("FIRST STDERR:\n{}", String::from_utf8_lossy(&first.stderr));
    assert!(first.status.success(), "first apply failed:\n{first:?}");

    let edited = fs::read_to_string(fixture.app_source()).unwrap();
    assert_ne!(edited, original, "cargo fix output:\n{first:?}");
    assert_eq!(
        edited.matches(MARKER).count(),
        26,
        "edited source:\n{edited}"
    );
    assert!(
        edited.contains("(42)"),
        "unrelated Rustfix edit leaked:\n{edited}"
    );
    assert!(edited.contains("pub const fn constant() -> i32 { 7 }"));
    assert!(edited.contains("pub extern \"C\" fn exported() -> i32 { 9 }"));

    // Explicit user instrumentation precedence: no duplicate markers on traced functions
    assert!(
        !edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"traced\")"),
        "traced() should not receive a P2.3 marker:\n{edited}"
    );
    assert!(
        !edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"traced_async\")"),
        "traced_async() should not receive a P2.3 marker:\n{edited}"
    );
    assert!(
        edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"ordinary_neighbor\")"),
        "ordinary_neighbor() adjacent to traced functions should be instrumented:\n{edited}"
    );

    // Nested local functions are intentionally excluded to maintain parity and prevent overlapping edits
    assert!(
        !edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"inner_local\")"),
        "nested inner_local() should not receive a P2.3 marker:\n{edited}"
    );
    assert!(
        !edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"inner_local_async\")"),
        "nested inner_local_async() should not receive a P2.3 marker:\n{edited}"
    );
    assert!(
        !edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"require_send\")"),
        "nested require_send() should not receive a P2.3 marker:\n{edited}"
    );
    assert!(
        edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"outer_with_nested\")"),
        "outer_with_nested() should be instrumented once:\n{edited}"
    );
    assert!(
        edited.contains(
            "Tracer::start(&__cargo_instrument_rust_tracer, \"assert_async_future_is_send\")"
        ),
        "assert_async_future_is_send() should be instrumented once:\n{edited}"
    );

    // Direct self-recursion is intentionally excluded per §12.3 parity to avoid recursive span explosion
    assert!(
        !edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"factorial\")"),
        "directly self-recursive factorial() should not receive a P2.3 marker:\n{edited}"
    );
    assert!(
        !edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"recursive_method\")"),
        "directly self-recursive recursive_method() should not receive a P2.3 marker:\n{edited}"
    );

    // C2: Precision proof: non-recursive function calling a same-named method on another type is NOT excluded
    // Both concrete source regions (method on Worker and free function) must be instrumented.
    let worker_method_body = edited
        .split("impl Worker {")
        .nth(1)
        .and_then(|s| s.split("pub fn work(&self)").nth(1))
        .and_then(|s| s.split("pub fn work(worker: &Worker)").next())
        .expect("Worker::work method body");
    assert!(
        worker_method_body.contains(MARKER),
        "Worker::work method must be instrumented:\n{worker_method_body}"
    );

    let free_work_body = edited
        .split("pub fn work(worker: &Worker)")
        .nth(1)
        .and_then(|s| s.split("pub trait PlainWorker").next())
        .expect("free work function body");
    assert!(
        free_work_body.contains(MARKER),
        "free work function must be instrumented:\n{free_work_body}"
    );

    // Trait implementation method policy:
    // 1. Plain trait impl method is instrumented
    assert!(
        edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"plain_work\")"),
        "plain_work() trait impl method should be instrumented:\n{edited}"
    );
    // 2. Default trait method body is intentionally excluded
    assert!(
        !edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"default_work\")"),
        "default trait method default_work() must not receive a P2.3 marker:\n{edited}"
    );
    // 3. Trait impl method with direct self-recursion is intentionally excluded
    assert!(
        !edited.contains("Tracer::start(&__cargo_instrument_rust_tracer, \"recursive_trait_method\")"),
        "directly self-recursive trait method recursive_trait_method() must not receive a P2.3 marker:\n{edited}"
    );

    // Part A: #[async_trait] source instrumentation
    let work_trait_body = edited
        .split("impl AsyncWorker for AsyncWorkerService")
        .nth(1)
        .and_then(|s| s.split("async fn work_traced").next())
        .expect("work_trait body");
    assert!(
        work_trait_body.contains(MARKER),
        "async_trait work_trait() must receive P2.3 marker:\n{work_trait_body}"
    );
    assert!(
        work_trait_body.contains("FutureExt::with_context(async move"),
        "async_trait work_trait() must use FutureExt::with_context:\n{work_trait_body}"
    );
    assert!(
        work_trait_body.contains("set_status(opentelemetry::trace::Status::error(\"\"))"),
        "async_trait work_trait() must set error status on Err:\n{work_trait_body}"
    );

    // Explicit instrumentation precedence under #[async_trait]:
    let work_traced_body = edited
        .split("impl AsyncWorker for AsyncWorkerService")
        .nth(1)
        .and_then(|s| s.split("async fn work_traced").nth(1))
        .and_then(|s| s.split("pub async fn caller_of_async_trait").next())
        .expect("work_traced body");
    assert!(
        !work_traced_body.contains(MARKER),
        "async_trait method with #[tracing::instrument] must not be double instrumented:\n{work_traced_body}"
    );

    // Caller of async_trait method is instrumented
    assert!(
        edited
            .contains("Tracer::start(&__cargo_instrument_rust_tracer, \"caller_of_async_trait\")"),
        "caller_of_async_trait() must be instrumented:\n{edited}"
    );

    // Result span-status instrumentation shape checks
    let sync_ok_body = edited
        .split("pub fn sync_ok")
        .nth(1)
        .and_then(|s| s.split("fn sync_step").next())
        .expect("sync_ok body");
    assert!(sync_ok_body.contains("let __cargo_instrument_rust_res: Result<_, _> = (|| {"));
    assert!(sync_ok_body.contains("if __cargo_instrument_rust_res.is_err() {"));
    assert!(sync_ok_body.contains("set_status(opentelemetry::trace::Status::error(\"\"))"));

    let async_ok_body = edited
        .split("pub async fn async_ok")
        .nth(1)
        .and_then(|s| s.split("async fn async_step").next())
        .expect("async_ok body");
    assert!(async_ok_body.contains("let __cargo_instrument_rust_res: Result<_, _> = opentelemetry::trace::FutureExt::with_context(async move {"));
    assert!(async_ok_body.contains("if __cargo_instrument_rust_res.is_err() {"));
    assert!(async_ok_body.contains("set_status(opentelemetry::trace::Status::error(\"\"))"));

    // Negative test: custom type named Result must NOT receive Result closure wrapping
    let custom_result_body = edited
        .split("pub fn custom_type_named_result")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .expect("custom_type_named_result body");
    assert!(
        !custom_result_body.contains("__cargo_instrument_rust_res"),
        "custom type named Result must not receive Result wrapping:\n{custom_result_body}"
    );

    // Reference return fallback: sync functions returning references fall back to prefix-only instrumentation
    let borrowed_body = edited
        .split("pub fn sync_returns_borrowed")
        .nth(1)
        .and_then(|s| s.split("pub fn sync_returns_mut_borrowed").next())
        .expect("sync_returns_borrowed body");
    assert!(
        !borrowed_body.contains("__cargo_instrument_rust_res"),
        "reference-returning sync function must fall back to prefix-only:\n{borrowed_body}"
    );
    let mut_borrowed_body = edited
        .split("pub fn sync_returns_mut_borrowed")
        .nth(1)
        .and_then(|s| s.split("pub fn factorial").next())
        .expect("sync_returns_mut_borrowed body");
    assert!(
        !mut_borrowed_body.contains("__cargo_instrument_rust_res"),
        "mut-reference-returning sync function must fall back to prefix-only:\n{mut_borrowed_body}"
    );

    let asynchronous = edited
        .split("pub async fn asynchronous")
        .nth(1)
        .and_then(|source| {
            source
                .split("\n}\n}\n\npub fn assert_async_future_is_send")
                .next()
        })
        .expect("instrumented async method body");
    assert!(asynchronous.contains("FutureExt::with_context(async move"));
    assert!(asynchronous.contains("std::future::ready(()).await"));
    assert!(
        !asynchronous.contains("_cargo_instrument_rust_guard"),
        "an async context guard must not cross await:\n{asynchronous}"
    );
    assert_eq!(
        fs::read_to_string(fixture.dependency_source()).unwrap(),
        dependency_before
    );
    assert_eq!(
        fs::read_to_string(fixture.nested_dependency_source()).unwrap(),
        nested_dependency_before,
        "an unselected path dependency below the selected package root was edited"
    );
    assert_eq!(
        fs::read_to_string(fixture.outside_source()).unwrap(),
        outside_source_before,
        "source outside the selected package root was edited"
    );
    fixture.stable_check();
    fixture.stable_test();

    // The production command refuses to edit over uncommitted user changes.
    let dirty_retry = fixture.run_apply();
    assert!(!dirty_retry.status.success());
    assert!(String::from_utf8_lossy(&dirty_retry.stderr).contains("dirty Git worktree"));

    fixture.commit("apply P2.3 instrumentation");
    let before_second_apply = fs::read_to_string(fixture.app_source()).unwrap();
    let second = fixture.run_apply();
    assert!(second.status.success(), "second apply failed:\n{second:?}");
    assert_eq!(
        fs::read_to_string(fixture.app_source()).unwrap(),
        before_second_apply
    );
}

struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        write(
            &temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nexclude = [\"external-dependency\", \"app/vendor/nested_dep\"]\nresolver = \"2\"\n",
        );
        write(
            &temp.path().join(".cargo/config.toml"),
            "[build]\nrustflags = [\"--cfg\", \"p23_fixture_cfg\", \"--force-warn=unused-parens\"]\nrustdocflags = [\"--cfg\", \"p23_fixture_cfg\"]\n",
        );
        write(&temp.path().join(".gitignore"), "/target\n");
        write(
            &temp.path().join("external-dependency/Cargo.toml"),
            "[package]\nname = \"external-dependency\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(
            &temp.path().join("external-dependency/src/lib.rs"),
            "pub fn plus_one(value: i32) -> i32 { value + 1 }\n",
        );
        write(
            &temp.path().join("app/vendor/nested_dep/Cargo.toml"),
            "[package]\nname = \"nested-dependency\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nopentelemetry = \"0.32.0\"\n",
        );
        write(
            &temp.path().join("app/vendor/nested_dep/src/lib.rs"),
            "pub fn nested(value: i32) -> i32 { value * 10 }\n",
        );
        write(
            &temp.path().join("shared/generated.rs"),
            "pub fn generated_outside_package() -> i32 { 11 }\n",
        );
        write(
            &temp.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nexternal-dependency = { path = \"../external-dependency\" }\nnested-dependency = { path = \"vendor/nested_dep\" }\nopentelemetry = \"0.32.0\"\ntracing = { version = \"0.1\", features = [\"attributes\"] }\nasync-trait = \"0.1\"\n\n[dev-dependencies]\nopentelemetry_sdk = { version = \"0.32.0\", features = [\"testing\"] }\n",
        );
        write(
            &temp.path().join("app/src/lib.rs"),
            r#"#![allow(dead_code)]
#[cfg(not(p23_fixture_cfg))]
compile_error!("the fixture Cargo rustflags must remain active");

#[path = "../../shared/generated.rs"]
pub mod generated;

pub fn sync(value: i32) -> i32 { external_dependency::plus_one(value) + nested_dependency::nested(0) }

pub fn unrelated_fixable_warning() -> i32 { (42) }

pub const fn constant() -> i32 { 7 }

pub extern "C" fn exported() -> i32 { 9 }

#[tracing::instrument]
pub fn traced() -> i32 { 10 }

#[tracing::instrument]
pub async fn traced_async() -> i32 {
    std::future::ready(()).await;
    20
}

pub fn ordinary_neighbor(value: i32) -> i32 {
    value + 5
}

pub fn outer_with_nested() -> i32 {
    fn inner_local(value: i32) -> i32 {
        value + 1
    }
    async fn inner_local_async() {
        std::future::ready(()).await;
    }
    let _ = inner_local_async();
    inner_local(41)
}

pub struct Service;

impl Service {
    pub fn method(&self, value: i32) -> i32 { value * 2 }

    pub async fn asynchronous(&self, value: i32) -> i32 {
        std::future::ready(()).await;
        value + 3
    }
}

pub fn assert_async_future_is_send() {
    fn require_send<T: Send>(_: T) {}
    require_send(Service.asynchronous(1));
}

// Goal A: Result/error span-status semantics
pub fn sync_ok() -> Result<i32, &'static str> {
    let value = sync_step(21)?;
    Ok(value * 2)
}

fn sync_step(value: i32) -> Result<i32, &'static str> {
    Ok(value)
}

pub fn sync_err() -> Result<i32, &'static str> {
    let _ = sync_step_err()?;
    Ok(0)
}

fn sync_step_err() -> Result<i32, &'static str> {
    Err("sync failure")
}

// Opaque error type implementing NEITHER Debug NOR Display.
// Compiling and recording an error status on this proves that telemetry never
// formats or leaks the error value.
pub struct OpaqueError;

pub fn sync_err_opaque() -> Result<i32, OpaqueError> {
    Err(OpaqueError)
}

pub async fn async_ok() -> Result<i32, &'static str> {
    std::future::ready(()).await;
    let value = async_step(50).await?;
    Ok(value * 2)
}

async fn async_step(value: i32) -> Result<i32, &'static str> {
    std::future::ready(()).await;
    Ok(value)
}

pub async fn async_err() -> Result<i32, &'static str> {
    std::future::ready(()).await;
    let _ = async_step_err().await?;
    Ok(0)
}

async fn async_step_err() -> Result<i32, &'static str> {
    std::future::ready(()).await;
    Err("async failure")
}

// Negative fixture: custom type named Result must NOT be treated as standard Result
pub mod custom {
    pub struct Result {
        pub code: i32,
    }

    pub fn custom_type_named_result() -> Result {
        Result { code: 42 }
    }
}
pub use custom::custom_type_named_result;

// Sync reference return fallback (§16.10 / C1 closure escape safety)
pub fn sync_returns_borrowed<'a>(input: &'a i32) -> std::result::Result<&'a i32, &'static str> {
    std::result::Result::Ok(input)
}

pub fn sync_returns_mut_borrowed<'a>(input: &'a mut i32) -> std::result::Result<&'a mut i32, &'static str> {
    *input += 1;
    std::result::Result::Ok(input)
}

// Goal B: direct self-recursion is intentionally excluded
pub fn factorial(n: u32) -> u32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

pub struct Counter {
    pub value: u32,
}

impl Counter {
    pub fn recursive_method(&self, n: u32) -> u32 {
        if n == 0 {
            self.value
        } else {
            self.recursive_method(n - 1)
        }
    }
}

// Precision check: a non-recursive function calling a same-named method on another type is NOT excluded
pub struct Worker;

impl Worker {
    pub fn work(&self) -> u32 {
        99
    }
}

pub fn work(worker: &Worker) -> u32 {
    worker.work()
}

pub trait PlainWorker {
    fn plain_work(&self, value: i32) -> i32;
    fn default_work(&self) -> i32 { 42 }
}

impl PlainWorker for Service {
    fn plain_work(&self, value: i32) -> i32 {
        value + 10
    }
}

pub trait RecursiveWorker {
    fn recursive_trait_method(&self, n: u32) -> u32;
}

impl RecursiveWorker for Service {
    fn recursive_trait_method(&self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            self.recursive_trait_method(n - 1)
        }
    }
}

pub struct YieldOnce {
    pub yielded: bool,
}

impl YieldOnce {
    pub fn new() -> Self {
        Self { yielded: false }
    }
}

impl std::future::Future for YieldOnce {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if self.yielded {
            std::task::Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

#[async_trait::async_trait]
pub trait AsyncWorker {
    async fn work_trait(&self, value: i32) -> Result<i32, &'static str>;
    async fn work_traced(&self, value: i32) -> Result<i32, &'static str>;
}

#[derive(Debug)]
pub struct AsyncWorkerService;

#[async_trait::async_trait]
impl AsyncWorker for AsyncWorkerService {
    async fn work_trait(&self, value: i32) -> Result<i32, &'static str> {
        YieldOnce::new().await;
        if value < 0 {
            Err("async_trait failure")
        } else {
            Ok(value + 1)
        }
    }

    #[tracing::instrument]
    async fn work_traced(&self, value: i32) -> Result<i32, &'static str> {
        YieldOnce::new().await;
        Ok(value * 2)
    }
}

pub async fn caller_of_async_trait(
    worker: &AsyncWorkerService,
    value: i32,
) -> Result<i32, &'static str> {
    worker.work_trait(value).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

    fn run_future<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        use std::pin::pin;
        fn noop_clone(_: *const ()) -> RawWaker { RawWaker::new(std::ptr::null(), &VTABLE) }
        fn noop(_: *const ()) {}
        static VTABLE: RawWakerVTable = RawWakerVTable::new(noop_clone, noop, noop, noop);
        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);
        let mut pinned = pin!(fut);
        loop {
            if let Poll::Ready(res) = pinned.as_mut().poll(&mut cx) {
                return res;
            }
        }
    }

    fn assert_send<T: Send>(_: &T) {}

    #[test]
    fn test_runtime_telemetry_and_result_status() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let _ = opentelemetry::global::set_tracer_provider(provider);

        // 1. Sync Ok returns unchanged value
        assert_eq!(sync_ok(), Ok(42));

        // 2. Sync Err returns unchanged value
        assert_eq!(sync_err(), Err("sync failure"));

        // 3. Sync Err with opaque error (no Debug/Display) returns unchanged value
        assert!(sync_err_opaque().is_err());

        // 4. Async Ok returns unchanged value and Future is Send
        let fut_ok = async_ok();
        assert_send(&fut_ok);
        assert_eq!(run_future(fut_ok), Ok(100));

        // 5. Async Err returns unchanged value and Future is Send
        let fut_err = async_err();
        assert_send(&fut_err);
        assert_eq!(run_future(fut_err), Err("async failure"));

        // 6. Custom type named Result
        let custom = custom_type_named_result();
        assert_eq!(custom.code, 42);

        // 7. Direct self-recursion executes correctly without telemetry explosion
        assert_eq!(factorial(5), 120);
        let counter = Counter { value: 7 };
        assert_eq!(counter.recursive_method(3), 7);

        // 8. Non-recursive call to helper with same method name
        assert_eq!(work(&Worker), 99);

        // 9. Plain trait method
        assert_eq!(Service.plain_work(5), 15);

        // 10. Default trait method
        assert_eq!(Service.default_work(), 42);

        // 11. Trait method direct recursion exclusion
        assert_eq!(Service.recursive_trait_method(3), 0);

        // 12. async_trait method execution with real suspension (YieldOnce: Pending -> wake -> Ready)
        let async_svc = AsyncWorkerService;

        // 12a. Caller -> async_trait method parenting proof
        let fut_parenting = caller_of_async_trait(&async_svc, 10);
        assert_send(&fut_parenting);
        assert_eq!(run_future(fut_parenting), Ok(11));

        // 12b. async_trait Err returns unchanged value, future is Send, status is Error("")
        let fut_trait_err = async_svc.work_trait(-5);
        assert_send(&fut_trait_err);
        assert_eq!(run_future(fut_trait_err), Err("async_trait failure"));

        // 12c. async_trait explicit instrumentation precedence (#[tracing::instrument])
        let fut_traced = async_svc.work_traced(21);
        assert_send(&fut_traced);
        assert_eq!(run_future(fut_traced), Ok(42));

        // Retrieve and assert finished span statuses
        let spans = exporter.get_finished_spans().expect("get finished spans");

        let find_span = |name: &str| {
            spans.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("missing span: {name}"))
        };

        let s_sync_ok = find_span("sync_ok");
        assert_eq!(s_sync_ok.status, opentelemetry::trace::Status::Unset, "sync_ok status must be Unset");

        let s_sync_err = find_span("sync_err");
        assert_eq!(s_sync_err.status, opentelemetry::trace::Status::error(""), "sync_err status must be Error with empty description");

        let s_opaque = find_span("sync_err_opaque");
        assert_eq!(s_opaque.status, opentelemetry::trace::Status::error(""), "opaque error must have Error status with empty description");

        let s_async_ok = find_span("async_ok");
        assert_eq!(s_async_ok.status, opentelemetry::trace::Status::Unset, "async_ok status must be Unset");

        let s_async_err = find_span("async_err");
        assert_eq!(s_async_err.status, opentelemetry::trace::Status::error(""), "async_err status must be Error with empty description");

        let s_custom = find_span("custom_type_named_result");
        assert_eq!(s_custom.status, opentelemetry::trace::Status::Unset, "custom Result must not have Error status");

        // Excluded direct recursion functions must not produce spans
        assert!(!spans.iter().any(|s| s.name == "factorial"), "factorial must not produce any span");
        assert!(!spans.iter().any(|s| s.name == "recursive_method"), "recursive_method must not produce any span");

        let s_plain = find_span("plain_work");
        assert_eq!(s_plain.status, opentelemetry::trace::Status::Unset);

        assert!(!spans.iter().any(|s| s.name == "default_work"), "default_work must not produce any span");
        assert!(!spans.iter().any(|s| s.name == "recursive_trait_method"), "recursive_trait_method must not produce any span");

        // C2: exact same-name recursion precision proof at runtime
        let work_spans: Vec<_> = spans.iter().filter(|s| s.name == "work").collect();
        assert_eq!(work_spans.len(), 2, "must produce exactly 2 'work' spans (one free, one method)");
        let parent_work = work_spans.iter().find(|s| s.parent_span_id == opentelemetry::trace::SpanId::INVALID).expect("parent work span");
        let child_work = work_spans.iter().find(|s| s.parent_span_id != opentelemetry::trace::SpanId::INVALID).expect("child work span");
        assert_eq!(child_work.parent_span_id, parent_work.span_context.span_id(), "method work must be child of free work");
        assert_eq!(child_work.span_context.trace_id(), parent_work.span_context.trace_id(), "method and free work must share trace_id");

        // async_trait caller-callee parenting proof
        let s_caller = find_span("caller_of_async_trait");
        let s_trait_ok = spans.iter().find(|s| s.name == "work_trait" && s.status == opentelemetry::trace::Status::Unset).expect("work_trait ok span");
        assert_eq!(s_trait_ok.span_context.trace_id(), s_caller.span_context.trace_id(), "async_trait span must share trace_id with caller");
        assert_eq!(s_trait_ok.parent_span_id, s_caller.span_context.span_id(), "async_trait span must have caller as parent_span_id");

        let s_trait_err = spans.iter().find(|s| s.name == "work_trait" && s.status != opentelemetry::trace::Status::Unset).expect("work_trait err span");
        assert_eq!(s_trait_err.status, opentelemetry::trace::Status::error(""), "async_trait error status must be Error with empty description");

        // Explicitly instrumented async_trait method must NOT produce a P2.3 span
        assert!(!spans.iter().any(|s| s.name == "work_traced"), "work_traced has #[tracing::instrument] and must not receive a P2.3 span");
    }
}
"#,
        );
        let fixture = Self { temp };
        fixture.run_git(&["init"]);
        fixture.run_git(&["add", "."]);
        fixture.commit("initial fixture");
        fixture
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn app_source(&self) -> std::path::PathBuf {
        self.root().join("app/src/lib.rs")
    }

    fn dependency_source(&self) -> std::path::PathBuf {
        self.root().join("external-dependency/src/lib.rs")
    }

    fn nested_dependency_source(&self) -> std::path::PathBuf {
        self.root().join("app/vendor/nested_dep/src/lib.rs")
    }

    fn outside_source(&self) -> std::path::PathBuf {
        self.root().join("shared/generated.rs")
    }

    fn run_apply(&self) -> std::process::Output {
        let binary = Path::new(env!("CARGO_BIN_EXE_cargo-instrument-rust"));
        let mut paths = vec![binary.parent().unwrap().to_path_buf()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let mut command = Command::new("cargo");
        command
            .args([
                "instrument-rust",
                "--apply",
                "--package",
                "app",
                "--offline",
            ])
            .current_dir(self.root())
            .env("PATH", std::env::join_paths(paths).unwrap());
        // Local developer images may expose the pinned compiler under the
        // rolling `nightly` alias. CI deliberately leaves this unset.
        if let Ok(toolchain) = std::env::var("P23_TEST_TOOLCHAIN") {
            command.env("CARGO_INSTRUMENT_RUST_TOOLCHAIN", toolchain);
        }
        command.output().unwrap()
    }

    fn stable_check(&self) {
        let output = Command::new("cargo")
            .args(["+stable", "check", "--workspace", "--offline"])
            .current_dir(self.root())
            .output()
            .unwrap();
        assert!(output.status.success(), "stable check failed:\n{output:?}");
    }

    fn stable_test(&self) {
        let output = Command::new("cargo")
            .args(["+stable", "test", "--package", "app", "--offline"])
            .current_dir(self.root())
            .output()
            .unwrap();
        assert!(output.status.success(), "stable test failed:\n{output:?}");
    }

    fn commit(&self, message: &str) {
        self.run_git(&["add", "."]);
        self.run_git(&[
            "-c",
            "user.name=P2.3 Fixture",
            "-c",
            "user.email=p23@example.invalid",
            "commit",
            "-m",
            message,
        ]);
    }

    fn run_git(&self, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(self.root())
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}
