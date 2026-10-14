//! Markdown-driven serverless backend SDK.
//!
//! Source markdown is compiled into structured lock files. The runtime loads a
//! signed manifest built from those locks and does not interpret natural language.

#![allow(clippy::result_large_err)]

pub mod buildrecord;
pub mod compile;
pub mod diag;
pub mod discover;
pub mod fsutil;
pub mod hashutil;
pub mod llm;
pub mod lock;
pub mod manifest;
pub mod runtime;
pub mod scan;
pub mod secret;
pub mod security;
pub mod sign;
pub mod sqlpolicy;
pub mod validate;

pub use compile::{compile_project, CompileOptions, CompileOutput};
pub use diag::{Diag, Level, Report};
pub use llm::{load_llm_config, FakeAdapter, HttpAdapter, LlmAdapter, LlmConfig};
pub use manifest::{manifest_from_project, read_manifest, write_manifest, Manifest};
pub use validate::{validate_project, ValidateOptions};
