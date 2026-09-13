//! The compiler-independent boundary shared by instrumentation frontends.
//!
//! This crate intentionally contains no syntax-tree or compiler-private types.

pub const P23_MARKER: &str = "/* __cargo_instrument_rust:p23 */";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunctionShape {
    FreeFunction,
    InherentMethod,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionFacts {
    pub function_name: String,
    pub crate_name: String,
    pub shape: FunctionShape,
    pub is_async: bool,
    pub first_party: bool,
    pub already_instrumented: bool,
    pub has_opentelemetry: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ineligibility {
    NotFirstParty,
    AlreadyInstrumented,
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
        if !facts.has_opentelemetry {
            return Err(Ineligibility::MissingOpenTelemetryDependency);
        }
        if !matches!(
            facts.shape,
            FunctionShape::FreeFunction | FunctionShape::InherentMethod
        ) {
            return Err(Ineligibility::UnsupportedFunctionShape);
        }

        Ok(InstrumentationPlan {
            marker: P23_MARKER,
            span: SpanSemantics {
                tracer_scope: facts.crate_name.clone(),
                span_name: facts.function_name.clone(),
            },
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
            first_party: true,
            already_instrumented: false,
            has_opentelemetry: true,
        }
    }

    #[test]
    fn makes_a_plan_for_supported_first_party_function() {
        let plan = EligibilityPolicy::plan(&facts()).unwrap();
        assert_eq!(plan.marker, P23_MARKER);
        assert_eq!(plan.span.span_name, "work");
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
}
