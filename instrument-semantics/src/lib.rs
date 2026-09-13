//! The compiler-independent boundary shared by instrumentation frontends.
//!
//! This crate intentionally contains no syntax-tree or compiler-private types.

pub const P23_MARKER: &str = "/* __cargo_instrument_rust:p23 */";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunctionShape {
    FreeFunction,
    InherentMethod,
    TraitImplMethod,
    DefaultTraitMethod,
    NestedLocalFunction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionFacts {
    pub function_name: String,
    pub crate_name: String,
    pub shape: FunctionShape,
    pub is_async: bool,
    pub returns_result: bool,
    pub can_capture_result_status: bool,
    pub is_directly_recursive: bool,
    pub first_party: bool,
    pub already_instrumented: bool,
    pub has_explicit_instrumentation: bool,
    pub has_opentelemetry: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ineligibility {
    NotFirstParty,
    AlreadyInstrumented,
    ExplicitlyInstrumented,
    NestedLocalFunctionExcluded,
    DirectSelfRecursionExcluded,
    DefaultTraitMethodExcluded,
    UnsupportedFunctionShape,
    MissingOpenTelemetryDependency,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpanSemantics {
    pub tracer_scope: String,
    pub span_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstrumentationPlan {
    pub marker: &'static str,
    pub span: SpanSemantics,
    pub lifecycle: ExecutionLifecycle,
    pub captures_result_status: bool,
}

/// The execution model selected by shared policy, not by an individual frontend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionLifecycle {
    SyncScopedContext,
    AsyncFutureContext,
}

pub struct EligibilityPolicy;

impl EligibilityPolicy {
    pub fn plan(facts: &FunctionFacts) -> Result<InstrumentationPlan, Ineligibility> {
        if !facts.first_party {
            return Err(Ineligibility::NotFirstParty);
        }
        if facts.already_instrumented {
            return Err(Ineligibility::AlreadyInstrumented);
        }
        if facts.has_explicit_instrumentation {
            return Err(Ineligibility::ExplicitlyInstrumented);
        }
        if facts.shape == FunctionShape::NestedLocalFunction {
            return Err(Ineligibility::NestedLocalFunctionExcluded);
        }
        if facts.shape == FunctionShape::DefaultTraitMethod {
            return Err(Ineligibility::DefaultTraitMethodExcluded);
        }
        if facts.is_directly_recursive {
            return Err(Ineligibility::DirectSelfRecursionExcluded);
        }
        if !facts.has_opentelemetry {
            return Err(Ineligibility::MissingOpenTelemetryDependency);
        }

        let lifecycle = if facts.is_async {
            ExecutionLifecycle::AsyncFutureContext
        } else {
            ExecutionLifecycle::SyncScopedContext
        };

        Ok(InstrumentationPlan {
            marker: P23_MARKER,
            span: SpanSemantics {
                tracer_scope: facts.crate_name.clone(),
                span_name: facts.function_name.clone(),
            },
            lifecycle,
            captures_result_status: facts.returns_result && facts.can_capture_result_status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> FunctionFacts {
        FunctionFacts {
            function_name: "work".into(),
            crate_name: "demo".into(),
            shape: FunctionShape::FreeFunction,
            is_async: false,
            returns_result: false,
            can_capture_result_status: true,
            is_directly_recursive: false,
            first_party: true,
            already_instrumented: false,
            has_explicit_instrumentation: false,
            has_opentelemetry: true,
        }
    }

    #[test]
    fn makes_a_plan_for_supported_first_party_function() {
        let plan = EligibilityPolicy::plan(&facts()).unwrap();
        assert_eq!(plan.marker, P23_MARKER);
        assert_eq!(plan.span.span_name, "work");
        assert_eq!(plan.lifecycle, ExecutionLifecycle::SyncScopedContext);
        assert!(!plan.captures_result_status);
    }

    #[test]
    fn marker_makes_an_edit_ineligible() {
        let mut facts = facts();
        facts.already_instrumented = true;
        assert_eq!(
            EligibilityPolicy::plan(&facts),
            Err(Ineligibility::AlreadyInstrumented)
        );
    }

    #[test]
    fn explicit_instrumentation_makes_an_edit_ineligible() {
        let mut facts = facts();
        facts.has_explicit_instrumentation = true;
        assert_eq!(
            EligibilityPolicy::plan(&facts),
            Err(Ineligibility::ExplicitlyInstrumented)
        );
    }

    #[test]
    fn nested_local_function_is_intentionally_excluded() {
        let mut facts = facts();
        facts.shape = FunctionShape::NestedLocalFunction;
        assert_eq!(
            EligibilityPolicy::plan(&facts),
            Err(Ineligibility::NestedLocalFunctionExcluded)
        );
    }

    #[test]
    fn nested_local_async_function_is_intentionally_excluded() {
        let mut facts = facts();
        facts.shape = FunctionShape::NestedLocalFunction;
        facts.is_async = true;
        assert_eq!(
            EligibilityPolicy::plan(&facts),
            Err(Ineligibility::NestedLocalFunctionExcluded)
        );
    }

    #[test]
    fn direct_self_recursion_is_intentionally_excluded() {
        let mut facts = facts();
        facts.is_directly_recursive = true;
        assert_eq!(
            EligibilityPolicy::plan(&facts),
            Err(Ineligibility::DirectSelfRecursionExcluded)
        );
    }

    #[test]
    fn default_trait_method_is_intentionally_excluded() {
        let mut facts = facts();
        facts.shape = FunctionShape::DefaultTraitMethod;
        assert_eq!(
            EligibilityPolicy::plan(&facts),
            Err(Ineligibility::DefaultTraitMethodExcluded)
        );
    }

    #[test]
    fn trait_impl_method_is_eligible() {
        let mut facts = facts();
        facts.shape = FunctionShape::TraitImplMethod;
        assert!(EligibilityPolicy::plan(&facts).is_ok());
    }

    #[test]
    fn returns_result_is_propagated_into_plan() {
        let mut facts = facts();
        facts.returns_result = true;
        let plan = EligibilityPolicy::plan(&facts).unwrap();
        assert!(plan.captures_result_status);
    }

    #[test]
    fn sync_reference_result_returns_result_true_but_captures_result_status_false() {
        let mut facts = facts();
        facts.returns_result = true;
        facts.can_capture_result_status = false;
        let plan = EligibilityPolicy::plan(&facts).unwrap();
        assert!(!plan.captures_result_status);
    }

    #[test]
    fn async_facts_select_per_poll_future_context() {
        let mut facts = facts();
        facts.is_async = true;
        assert_eq!(
            EligibilityPolicy::plan(&facts).unwrap().lifecycle,
            ExecutionLifecycle::AsyncFutureContext
        );
    }
}
