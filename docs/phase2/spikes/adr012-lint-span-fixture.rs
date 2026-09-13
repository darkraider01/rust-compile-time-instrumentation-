// HISTORICAL SPIKE FIXTURE — NOT PRODUCTION TEST COVERAGE.
//
// Input for `adr012-lint-span-probe.rs`; retained as ADR-012 evidence only.

pub fn plain_sync(a: i32) -> i32 {
    a + 1
}

pub async fn plain_async(a: i32) -> i32 {
    a + 2
}

pub struct S;

impl S {
    pub fn inherent(&self, a: i32) -> i32 {
        a + 3
    }
}

#[async_trait::async_trait]
pub trait T {
    async fn via_async_trait(&self, a: i32) -> i32;
}

#[async_trait::async_trait]
impl T for S {
    async fn via_async_trait(&self, a: i32) -> i32 {
        a + 4
    }
}

macro_rules! make_fn {
    ($n:ident) => {
        pub fn $n(a: i32) -> i32 {
            a + 5
        }
    };
}
make_fn!(from_macro);
