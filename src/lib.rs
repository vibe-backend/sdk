//! Markdown-driven serverless backend SDK.
//!
//! Source markdown is compiled into structured lock files. The runtime loads a
//! signed manifest built from those locks and does not interpret natural language.

#![allow(clippy::result_large_err)]

#[cfg(feature = "sdk")]
pub mod buildrecord;
#[cfg(feature = "sdk")]
pub mod compile;
pub mod diag;
pub mod discover;
pub mod fsutil;
pub mod hashutil;
#[cfg(feature = "sdk")]
pub mod llm;
#[cfg(feature = "sdk")]
pub mod llm_budget;
pub mod lock;
#[cfg(feature = "sdk")]
pub mod lockcommit;
pub mod manifest;
pub mod netguard;
pub mod project;
#[cfg(feature = "sdk")]
pub mod prompt_guide;
pub mod runtime;
#[cfg(feature = "sdk")]
pub mod scaffold;
#[cfg(feature = "sdk")]
pub mod scan;
#[cfg(feature = "sdk")]
pub mod secret;
#[cfg(feature = "sdk")]
pub mod security;
pub mod sign;
pub mod sigstore;
#[cfg(feature = "sdk")]
pub mod source_lines;
pub mod sqlguard;
pub mod sqlpolicy;
#[cfg(feature = "sdk")]
pub mod validate;

#[cfg(feature = "sdk")]
pub use compile::{build_project, regenerate_project, CompileOptions, CompileOutput};
pub use diag::{Diag, Level, Report};
#[cfg(feature = "sdk")]
pub use llm::{load_llm_config, FakeAdapter, HttpAdapter, LlmAdapter, LlmConfig};
#[cfg(feature = "sdk")]
pub use manifest::manifest_from_project;
pub use manifest::{read_manifest, write_manifest, Manifest};
#[cfg(feature = "sdk")]
pub use validate::{validate_project, ValidateOptions};

#[cfg(feature = "sdk")]
pub mod interactive;

#[cfg(feature = "sdk")]
pub(crate) mod zh;

pub mod run;

pub mod duration;
pub mod runtime_env;

#[cfg(feature = "sdk")]
pub mod global_config;

#[cfg(feature = "sdk")]
pub mod llm_replay;
