//! Build record: `vibe compile` writes the SHA-256 of every lock it produced
//! to `.vibe/build-record.json`. Deploy validation and `vibe manifest`
//! require each lock to match it byte for byte, so a lock edited after the
//! build (natural-language locks included, which cannot be recompiled
//! offline) is refused.
//!
//! This is a local tripwire, not a trust root: the record is not signed, and
//! someone who can rewrite it can also rewrite the locks. Deployments take
//! the CI-built, attested artifact instead (see docs/ci/README.md).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::diag::{Diag, Report};
use crate::hashutil::sha256_prefixed;
use crate::validate::LoadedProject;

pub const PATH: &str = ".vibe/build-record.json";
const VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct BuildRecord {
    pub version: u32,
    /// Lock path (relative to the project) -> `sha256:<hex>` of its text.
    pub locks: BTreeMap<String, String>,
}

pub fn lock_hash(text: &str) -> String {
    sha256_prefixed(text.as_bytes())
}

/// Writes the record atomically (temp file, fsync, rename, fsync dir).
pub fn write(root: &Path, locks: BTreeMap<String, String>) -> Result<(), String> {
    let record = BuildRecord {
        version: VERSION,
        locks,
    };
    let json = serde_json::to_vec_pretty(&record).map_err(|err| err.to_string())?;
    crate::fsutil::write_atomic(&root.join(PATH), &json).map_err(|err| err.to_string())
}

pub fn check(loaded: &LoadedProject, report: &mut Report) {
    let Some(raw) = &loaded.build_record else {
        report.push(Diag::error(
            PATH,
            "no build record; locks must come from `vibe compile` (or the CI build)",
        ));
        return;
    };
    let record: BuildRecord = match serde_json::from_str::<BuildRecord>(raw) {
        Ok(record) if record.version == VERSION => record,
        _ => {
            report.push(Diag::error(
                PATH,
                "build record cannot be read; run `vibe compile`",
            ));
            return;
        }
    };
    for (source, text) in loaded.sources.iter().zip(&loaded.lock_texts) {
        let Some(text) = text else { continue };
        let rel = source.rel.trim_end_matches(".md").to_string() + ".lock.md";
        match record.locks.get(&rel) {
            Some(hash) if *hash == lock_hash(text) => {}
            Some(_) => report.push(Diag::error(
                &rel,
                "lock differs from what `vibe compile` wrote (hand-edited?); run `vibe compile`",
            )),
            None => report.push(Diag::error(
                &rel,
                "lock is not in the build record; run `vibe compile`",
            )),
        }
    }
}
