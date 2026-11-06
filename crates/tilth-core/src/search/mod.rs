//! Cross-file queries over the parsed tree: the shared walker, call sites,
//! callees, enclosing scopes and file-level dependencies.

pub mod bloom_walk;
mod callee_query;
pub mod callees;
pub mod callers;
pub mod deps;
pub mod scope;
pub mod walk;
