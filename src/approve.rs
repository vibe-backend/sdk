use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;

use crate::diag::{Diag, Report};
use crate::lock::{render_lock, ApprovalMode, SourceType, Status};
use crate::sign;
use crate::validate::{load_project, validate_loaded, ValidateOptions};

pub struct ApproveOptions<'a> {
    pub root: &'a Path,
    pub reviewers: &'a [String],
    pub reason: &'a str,
    pub approval_ref: &'a str,
    pub commit: &'a str,
    pub now: &'a str,
    pub signing_key: &'a SigningKey,
}

struct Prepared {
    path: PathBuf,
    /// Exact bytes that were validated; the file must still hold them.
    original: String,
    signed: String,
}

pub fn approve_project(opts: &ApproveOptions<'_>) -> Result<Report, Report> {
    approve_project_with_hook(opts, || {})
}

/// [`approve_project`] with a hook that runs after validation and before
/// anything is written (tests use it to change a lock in between).
#[doc(hidden)]
pub fn approve_project_with_hook(
    opts: &ApproveOptions<'_>,
    after_validate: impl FnOnce(),
) -> Result<Report, Report> {
    let mut report = Report::new();
    if opts.reason.trim().is_empty() {
        report.push(Diag::error("approve", "a reason is required"));
        return Err(report);
    }

    // Read every lock once, validate those bytes, and sign exactly the docs
    // parsed from them (#34). Approval and signatures are not checked: they
    // are what this command is about to write.
    let loaded = load_project(opts.root)?;
    let (validated, mut docs) = validate_loaded(
        &ValidateOptions {
            root: opts.root,
            deploy: false,
            verify_key: None,
        },
        &loaded,
    );
    match validated {
        Ok(found) => report.extend(found.diags), // warnings are shown, not dropped
        Err(problems) => {
            report.push(Diag::error(
                "approve",
                "validation failed; fix these errors (see vibe validate) before approving",
            ));
            report.extend(problems.diags);
            return Err(report);
        }
    }
    after_validate();

    let mut prepared = Vec::new();
    for (source, text) in loaded.sources.iter().zip(&loaded.lock_texts) {
        let path = opts.root.join(lock_rel(&source.rel));
        let (Some(text), Some(mut doc)) = (text, docs.remove(&source.id)) else {
            report.push(Diag::error(
                &source.rel,
                "lock is missing; run vibe compile first",
            ));
            continue;
        };
        let high: Vec<&str> = doc
            .meta
            .security_items
            .iter()
            .filter(|item| item.risk == crate::lock::Risk::High)
            .map(|item| item.item.as_str())
            .collect();
        let auto =
            doc.meta.source_type == SourceType::Structured && doc.meta.security_items.is_empty();
        if auto {
            doc.meta.approval_mode = ApprovalMode::Auto;
            doc.meta.approved_by = vec!["ci-auto".into()];
        } else {
            let needed = if high.is_empty() { 1 } else { 2 };
            if opts.reviewers.len() < needed {
                report.push(Diag::error(
                    &source.rel,
                    reviewer_error(&high, opts.reviewers.len()),
                ));
                continue;
            }
            doc.meta.approval_mode = ApprovalMode::Manual;
            doc.meta.approved_by = opts.reviewers.to_vec();
        }
        doc.meta.status = Status::Approved;
        doc.meta.approved_at = opts.now.into();
        doc.meta.approval_ref = opts.approval_ref.into();
        doc.meta.approved_commit = opts.commit.into();
        let approvers = doc.meta.approved_by.clone();
        for item in &mut doc.meta.security_items {
            if item.reason.is_empty() {
                item.reason = opts.reason.into();
            }
            item.approved_by = approvers.clone();
        }
        doc.meta.signature = "(none)".into();
        let signed = sign::sign_lock(&render_lock(&doc), opts.signing_key);
        prepared.push(Prepared {
            path,
            original: text.clone(),
            signed,
        });
    }
    if report.has_errors() {
        return Err(report);
    }
    write_all(&prepared, &mut report)?;
    report.into_result()
}

/// Writes all signed locks or none: every lock must still hold the bytes
/// that were validated, each new lock goes to a temp file next to it, and
/// only then are the temp files renamed over the originals (rename is
/// atomic per file, so no lock is ever left half written).
fn write_all(prepared: &[Prepared], report: &mut Report) -> Result<(), Report> {
    let fail = |report: &mut Report, path: &Path, message: String| {
        report.push(Diag::error(path.display().to_string(), message));
        Err(std::mem::take(report))
    };
    for item in prepared {
        match fs::read_to_string(&item.path) {
            Ok(now) if now == item.original => {}
            _ => {
                return fail(
                    report,
                    &item.path,
                    "lock changed after validation; nothing was signed, run vibe approve again"
                        .into(),
                )
            }
        }
    }
    let mut temps: Vec<PathBuf> = Vec::new();
    let cleanup = |temps: &[PathBuf]| {
        for temp in temps {
            let _ = fs::remove_file(temp);
        }
    };
    for item in prepared {
        let name = item
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temp = item
            .path
            .with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
        let written = fs::File::create(&temp).and_then(|mut file| {
            use std::io::Write;
            file.write_all(item.signed.as_bytes())?;
            file.sync_all()
        });
        temps.push(temp);
        if let Err(err) = written {
            cleanup(&temps);
            return fail(report, &item.path, format!("cannot write lock: {err}"));
        }
    }
    for (item, temp) in prepared.iter().zip(&temps) {
        if let Err(err) = fs::rename(temp, &item.path) {
            cleanup(&temps);
            return fail(report, &item.path, format!("cannot replace lock: {err}"));
        }
    }
    Ok(())
}

fn reviewer_error(high: &[&str], got: usize) -> String {
    if high.is_empty() {
        return format!("this lock needs 1 reviewer, got {got}; pass --reviewer");
    }
    let mut labels = Vec::new();
    for item in high {
        let label = match *item {
            "public" | "auth-removed" | "public-writes" | "password-write"
            | "verification-write" | "external-calls" | "jwt-config" => "public endpoint",
            "provider-new" => "provider",
            "migration-ddl" => "migration",
            "sign-token" | "set-password" | "confirm-verification" => "capability",
            "external-call" => "external call",
            "ownership" => "ownership",
            _ => "high risk",
        };
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    format!(
        "high-risk lock ({}) needs 2 reviewers, got {got}; pass --reviewer twice",
        labels.join(", ")
    )
}

fn lock_rel(source_rel: &str) -> String {
    source_rel.trim_end_matches(".md").to_string() + ".lock.md"
}
