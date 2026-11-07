//! Infrastructure adapters for external provider protocols.
pub mod gemini_embedding;
pub mod python_embedding;
pub mod python_reranker;

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod memory_backup;
