//! Memory planner for Nexum (`nx-mem`).
//!
//! Decides per function-local binding how it is stored:
//! - `Stack`: a scalar, held in a register or a typed stack slot. No
//!   buffer to own, so no free is ever emitted and nothing can dangle.
//! - `Unique`: owns heap buffers, single owner, freed at function exit
//!   (all `ret` paths) and before reassignment.
//! - `Shared`: escapes (globals, returns, retained params, aliases,
//!   ambiguous cases) -- lives for the process lifetime, as before.
//!
//! Soundness rule: doubt means Shared. A wrong Unique would be
//! use-after-free; a wrong Shared only costs memory. `Stack` is only
//! chosen when the value is provably a scalar, so it can never dangle.
//!
//! v0 limits: params are Shared unless proven scalar (caller owns the
//! buffers); for-each loop vars are Shared (they alias list elements);
//! analysis is per function with a fixpoint over retains-summaries; no
//! cross-module inference beyond direct same-module calls (unknown
//! callees retain).
//!
//! Layout: `types` holds the public model, `plan` the driver, `escape` the
//! escape analysis. `nx-codegen` calls `nx_mem::plan` and reads `nx_mem::Plan`.

mod escape;
mod plan;
mod types;

#[cfg(test)]
mod tests;

pub use plan::plan;
pub use types::{Alloc, Plan};
