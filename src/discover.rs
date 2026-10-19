use std::fs;
use std::path::{Path, PathBuf};

use crate::diag::{Diag, Report};
use crate::lock::{Kind, Scope};

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub rel: String,
    pub abs: PathBuf,
    pub kind: Kind,
    pub scope: Option<Scope>,
    pub id: String,
    pub text: String,
    pub schema: Option<String>,
}

impl SourceFile {
    pub fn combined_text(&self) -> String {
        match &self.schema {
            Some(schema) => format!("{}\n\n<!-- schema.md -->\n{schema}", self.text.trim_end()),
            None => self.text.clone(),
        }
    }

    pub fn lock_rel(&self) -> String {
        let path = Path::new(&self.rel);
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("md") => {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
                let parent = path.parent().unwrap_or(Path::new(""));
                parent
                    .join(format!("{stem}.lock.md"))
                    .to_string_lossy()
                    .replace('\\', "/")
            }
            _ => format!("{}.lock.md", self.rel),
        }
    }
}

pub fn discover(root: &Path) -> Result<Vec<SourceFile>, Report> {
    let mut report = Report::new();
    let mut files = Vec::new();
    collect_dir(&mut report, &mut files, root, root.join("providers"), 0);
    collect_dir(&mut report, &mut files, root, root.join("pipelines"), 0);
    collect_dir(&mut report, &mut files, root, root.join("migrations"), 0);
    collect_dir(&mut report, &mut files, root, root.join("models"), 0);
    collect_dir(&mut report, &mut files, root, root.join("api"), 0);
    report.diags.sort_by(|a, b| a.file.cmp(&b.file));
    if report.has_errors() {
        return Err(report);
    }
    attach_schemas(&mut files);
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(files)
}

fn collect_dir(
    report: &mut Report,
    files: &mut Vec<SourceFile>,
    root: &Path,
    dir: PathBuf,
    depth: usize,
) {
    if !dir.exists() {
        return;
    }
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) => {
            report.push(Diag::error(
                display_rel(root, &dir),
                format!("cannot read directory: {err}"),
            ));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 6 {
                report.push(Diag::error(
                    display_rel(root, &path),
                    "directory is too deep",
                ));
                continue;
            }
            collect_dir(report, files, root, path, depth + 1);
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = display_rel(root, &path);
        let parts: Vec<_> = rel.split('/').collect();
        if parts.first() == Some(&"api") && parts.len() >= 5 {
            report.push(old_layout(&rel, &parts));
            continue;
        }
        if let Some(stem) = name.strip_suffix(".schema.md") {
            let endpoint = path.with_file_name(format!("{stem}.md"));
            if !endpoint.is_file() {
                report.push(Diag::error(
                    &rel,
                    format!(
                        "schema has no matching endpoint file: {}",
                        display_rel(root, &endpoint)
                    ),
                ));
            }
            continue;
        }
        if !name.ends_with(".md") || name.ends_with(".lock.md") {
            continue;
        }
        match classify(root, &path) {
            Ok(file) => files.push(file),
            Err(diag) => report.push(diag),
        }
    }
}

fn attach_schemas(files: &mut [SourceFile]) {
    for file in files.iter_mut() {
        if file.kind != Kind::Api || file.scope != Some(Scope::Feature) {
            continue;
        }
        let schema_path = file.abs.file_stem().map(|stem| {
            file.abs
                .with_file_name(format!("{}.schema.md", stem.to_string_lossy()))
        });
        if let Some(path) = schema_path {
            if path.exists() {
                if let Ok(text) = fs::read_to_string(&path) {
                    file.schema = Some(text);
                }
            }
        }
    }
}

fn classify(root: &Path, path: &Path) -> Result<SourceFile, Diag> {
    let rel = display_rel(root, path);
    let parts: Vec<&str> = rel.split('/').collect();
    let text = fs::read_to_string(path)
        .map_err(|err| Diag::error(&rel, format!("cannot read source: {err}")))?;
    let classified = classify_parts(&rel, &parts)?;
    Ok(SourceFile {
        rel,
        abs: path.to_path_buf(),
        kind: classified.kind,
        scope: classified.scope,
        id: classified.id,
        text,
        schema: None,
    })
}

#[cfg(feature = "sdk")]
pub(crate) fn check_source_path(rel: &str) -> Result<(), Diag> {
    classify_parts(rel, &rel.split('/').collect::<Vec<_>>()).map(|_| ())
}

fn classify_parts(rel: &str, parts: &[&str]) -> Result<Classified, Diag> {
    let classified = match parts {
        ["providers", file] => simple_file(KindHint::NoneScope, file, "providers")?,
        ["pipelines", file] => simple_file(KindHint::PipelineWrap, file, "pipelines")?,
        ["migrations", provider, file] => classify_migration(provider, file)?,
        ["models", object, file] => classify_model(object, file)?,
        ["models", object, "methods", file] => classify_method(object, file)?,
        ["api", definition, file] => classify_definition(definition, file)?,
        ["api", definition, version, file] => classify_version(definition, version, file)?,
        ["api", _, _, _, ..] => return Err(old_layout(rel, parts)),
        _ => {
            return Err(Diag::error(
                rel,
                "path does not match providers, pipelines, migrations, models, or api/{definition}/{version}/{endpoint}.md",
            ));
        }
    };
    Ok(classified)
}

struct Classified {
    kind: Kind,
    scope: Option<Scope>,
    id: String,
}

enum KindHint {
    NoneScope,
    PipelineWrap,
}

fn simple_file(hint: KindHint, file: &str, folder: &str) -> Result<Classified, Diag> {
    let stem = stem_md(file).map_err(|message| Diag::error(format!("{folder}/{file}"), message))?;
    check_kebab(&stem).map_err(|message| Diag::error(format!("{folder}/{file}"), message))?;
    let kind = match hint {
        KindHint::NoneScope => Kind::Provider,
        KindHint::PipelineWrap => Kind::Pipeline,
    };
    Ok(Classified {
        kind,
        scope: None,
        id: stem,
    })
}

fn classify_migration(provider: &str, file: &str) -> Result<Classified, Diag> {
    let rel = format!("migrations/{provider}/{file}");
    check_kebab(provider).map_err(|message| Diag::error(&rel, message))?;
    let stem = stem_md(file).map_err(|message| Diag::error(&rel, message))?;
    let (seq, name) = stem
        .split_once('-')
        .ok_or_else(|| Diag::error(&rel, "migration file must look like 0001-name.md"))?;
    if seq.len() != 4 || !seq.chars().all(|c| c.is_ascii_digit()) {
        return Err(Diag::error(&rel, "migration sequence must be four digits"));
    }
    check_kebab(name).map_err(|message| Diag::error(&rel, message))?;
    Ok(Classified {
        kind: Kind::Migration,
        scope: None,
        id: format!("migrations/{provider}/{stem}"),
    })
}

fn classify_model(object: &str, file: &str) -> Result<Classified, Diag> {
    let rel = format!("models/{object}/{file}");
    check_kebab(object).map_err(|message| Diag::error(&rel, message))?;
    let stem = stem_md(file).map_err(|message| Diag::error(&rel, message))?;
    if stem != object {
        return Err(Diag::error(
            &rel,
            "model file name must match the folder name",
        ));
    }
    Ok(Classified {
        kind: Kind::Model,
        scope: None,
        id: object.into(),
    })
}

fn classify_method(object: &str, file: &str) -> Result<Classified, Diag> {
    let rel = format!("models/{object}/methods/{file}");
    check_kebab(object).map_err(|message| Diag::error(&rel, message))?;
    let stem = stem_md(file).map_err(|message| Diag::error(&rel, message))?;
    check_kebab(&stem).map_err(|message| Diag::error(&rel, message))?;
    if stem.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err(Diag::error(&rel, "method name must be kebab-case"));
    }
    Ok(Classified {
        kind: Kind::Method,
        scope: None,
        id: format!("{object}/methods/{stem}"),
    })
}

fn classify_definition(definition: &str, file: &str) -> Result<Classified, Diag> {
    let rel = format!("api/{definition}/{file}");
    check_kebab(definition).map_err(|message| Diag::error(&rel, message))?;
    if file != "_definition.md" {
        return Err(Diag::error(
            &rel,
            "only _definition.md is allowed directly under an api definition",
        ));
    }
    Ok(Classified {
        kind: Kind::Api,
        scope: Some(Scope::Definition),
        id: format!("{definition}/_definition"),
    })
}

fn classify_version(definition: &str, version: &str, file: &str) -> Result<Classified, Diag> {
    let rel = format!("api/{definition}/{version}/{file}");
    check_kebab(definition).map_err(|message| Diag::error(&rel, message))?;
    check_version(version).map_err(|message| Diag::error(&rel, message))?;
    let (scope, stem) = if file == "_version.md" {
        (Scope::Version, "_version".to_string())
    } else {
        let stem = stem_md(file).map_err(|message| Diag::error(&rel, message))?;
        check_kebab(&stem).map_err(|message| Diag::error(&rel, message))?;
        (Scope::Feature, stem)
    };
    Ok(Classified {
        kind: Kind::Api,
        scope: Some(scope),
        id: format!("{definition}/{version}/{stem}"),
    })
}

fn old_layout(rel: &str, parts: &[&str]) -> Diag {
    let base = format!("api/{}/{}", parts[1], parts[2]);
    let feature = parts[3];
    let action = if parts.len() == 5 && parts[4] == format!("{feature}.md") {
        format!("move it to {base}/{feature}.md")
    } else if parts.len() == 5 && parts[4] == "schema.md" {
        format!("move it to {base}/{feature}.schema.md")
    } else if parts.len() == 5 && parts[4] == format!("{feature}.lock.md") {
        format!("delete it; `vibe build` writes {base}/{feature}.lock.md")
    } else {
        format!(
            "endpoint folders are no longer supported; move endpoint sources to {base}/<name>.md"
        )
    };
    Diag::error(
        rel,
        format!("uses the old endpoint folder layout (decision 186): {action}"),
    )
}

fn stem_md(file: &str) -> Result<String, String> {
    file.strip_suffix(".md")
        .filter(|stem| !stem.is_empty() && !stem.contains('.'))
        .map(|stem| stem.to_string())
        .ok_or_else(|| format!("{file} is not a source markdown file"))
}

pub fn check_kebab(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!("{name} must be kebab-case (a-z, 0-9, hyphen)"))
    }
}

pub fn check_version(name: &str) -> Result<(), String> {
    let rest = name
        .strip_prefix('v')
        .ok_or_else(|| format!("{name} must look like v1"))?;
    if rest.starts_with('0') || rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("{name} must match ^v[1-9][0-9]*$"));
    }
    Ok(())
}

fn display_rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
