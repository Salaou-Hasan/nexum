//! Nexum HIR: name-free, typed, resolved high-level IR.
//!
//! Design: `docs/architecture/hir.md`. Model here, lowering in
//! [`lower`], verification in [`verify`], text dump in [`dump`].

pub mod dump;
pub mod lower;
pub mod model;
pub mod verify;

pub use model::*;
