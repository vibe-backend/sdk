use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;

use crate::diag::{Diag, Report};
use crate::discover;
use crate::lock::{parse_lock, render_lock, ApprovalMode, LockDoc, SourceType, Status};
use crate::sign;

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
    doc: LockDoc,
}

pub fn approve_project(opts: &ApproveOptions<'_>) -> Result<Report, Report> {
    let sources = discover::discover(opts.root)?;
    let mut report = Report::new();
    if opts.reason.trim().is_empty() {
        report.push(Diag::error("approve", "a reason is required"));
    }
    if report.has_errors() {
        return Err(report);
    }

    // Sign only what `vibe validate` accepts (#34). Approval and signatures
    // are not checked here: they are what this command is about to write.
    if let Err(problems) = crate::validate::validate_project(&crate::validate::ValidateOptions {
        root: opts.root,
        deploy: false,
        verify_key: None,
    }) {
        report.push(Diag::error(
            "approve",
            "validation failed; fix these errors (see vibe validate) before approving",
        ));
        report.extend(problems.diags);
        return Err(report);
    }

    let mut prepared = Vec::new();
    for source in sources {
        let path = opts.root.join(lock_rel(&source.rel));
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => {
                report.push(Diag::error(
                    &source.rel,
                    "lock is missing; run vibe compile first",
                ));
                continue;
            }
        };
        let mut doc = match parse_lock(&text) {
            Ok(doc) => doc,
            Err(err) => {
                report.push(Diag::error(lock_rel(&source.rel), err));
                continue;
            }
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
        prepared.push(Prepared { path, doc });
    }
    if report.has_errors() {
        return Err(report);
    }
    for item in &prepared {
        let unsigned = render_lock(&item.doc);
        let signed = sign::sign_lock(&unsigned, opts.signing_key);
        if let Err(err) = fs::write(&item.path, signed) {
            report.push(Diag::error(
                item.path.display().to_string(),
                format!("cannot write lock: {err}"),
            ));
            return Err(report);
        }
    }
    report.into_result()
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
