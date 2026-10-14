use std::fs;
use std::path::Path;

use crate::diag::{Diag, Report};

pub fn secret_scan(root: &Path) -> Report {
    let mut report = Report::new();
    let gitignore = root.join(".gitignore");
    match fs::read_to_string(&gitignore) {
        Ok(text) if text.lines().any(|line| line.trim() == ".env") => {}
        Ok(_) => report.push(Diag::error(".gitignore", ".env must stay gitignored")),
        Err(_) => report.push(Diag::error(".gitignore", ".gitignore is missing")),
    }
    let needle = std::env::var("LLM_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .ok()
        .filter(|value| value.len() >= 12 && value != "replace-me");
    walk(root, root, needle.as_deref(), &mut report);
    report
}

fn walk(root: &Path, dir: &Path, needle: Option<&str>, report: &mut Report) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name == ".git" || name == "target" || name == ".vibe" {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, needle, report);
            continue;
        }
        if name == ".env" {
            report.push(Diag::error(rel(root, &path), ".env must not be committed"));
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if text.contains('\0') {
            continue;
        }
        if let Some(secret) = needle {
            if text.contains(secret) {
                report.push(Diag::error(
                    rel(root, &path),
                    "file contains the configured LLM API key",
                ));
            }
        }
        if looks_like_secret(&text) {
            report.push(Diag::error(rel(root, &path), "possible secret material"));
        }
    }
}

fn looks_like_secret(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .any(|token| {
            let openai = token.starts_with("sk-") && token.len() >= 28;
            let aws = token.starts_with("AKIA") && token.len() == 20;
            openai || aws
        })
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
