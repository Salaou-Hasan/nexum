//! Public model for the memory plan: `Alloc` and `Plan`.
//!
//! `Alloc` is the per-binding storage decision consumed by `nx-codegen`
//! unboxing pass; `Plan` is the whole-program table built by `plan`.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alloc {
    /// Scalar: register or typed stack slot, never freed.
    Stack,
    Unique,
    Shared,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// (module, function, variable) -> allocation strategy.
    pub locals: HashMap<(String, String, String), Alloc>,
    /// (module, function) -> true if the function may retain its params.
    pub retains: HashMap<(String, String), bool>,
}

impl Plan {
    pub fn alloc_of(&self, module: &str, func: &str, var: &str) -> Alloc {
        self.locals
            .get(&(module.to_string(), func.to_string(), var.to_string()))
            .copied()
            .unwrap_or(Alloc::Shared)
    }
}
