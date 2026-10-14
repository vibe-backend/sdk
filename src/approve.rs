use std::fs;
use std::path::Path;

use ed25519_dalek::SigningKey;

use crate::diag::{Diag, Report};
use crate::discover;
use crate::lock::{parse_lock, render_lock, ApprovalMode, SourceType, Status};
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

pub fn approve_project(opts: &ApproveOptions<'_>) -> Result<Report, Report> {
    let sources = discover::discover(opts.root)?;
    let mut report = Report::new();
    if opts.reason.trim().is_empty() {
        report.push(Diag::error("approve", "a reason is required"));
    }
    if report.has_errors() {
        return Err(report);
    }
    for source in sources {
        let path = opts.root.join(lock_rel(&source.rel));
        let text = fs::read_to_string(&path)
            .map_err(|_| Diag::error(&source.rel, "lock is missing; run vibe compile first"));
        let text = match text {
            Ok(text) => text,
            Err(diag) => {
                report.push(diag);
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
        let high = doc
            .meta
            .security_items
            .iter()
            .any(|item| item.risk == crate::lock::Risk::High);
        let auto =
            doc.meta.source_type == SourceType::Structured && doc.meta.security_items.is_empty();
        if auto {
            doc.meta.approval_mode = ApprovalMode::Auto;
            doc.meta.approved_by = vec!["ci-auto".into()];
        } else {
            let needed = if high { 2 } else { 1 };
            if opts.reviewers.len() < needed {
                report.push(Diag::error(
                    &source.rel,
                    format!("this lock needs {needed} reviewers"),
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
        let unsigned = render_lock(&doc);
        let signed = sign::sign_lock(&unsigned, opts.signing_key);
        if let Err(err) = fs::write(&path, signed) {
            report.push(Diag::error(
                lock_rel(&source.rel),
                format!("cannot write lock: {err}"),
            ));
        }
    }
    report.into_result()
}

fn lock_rel(source_rel: &str) -> String {
    source_rel.trim_end_matches(".md").to_string() + ".lock.md"
}
