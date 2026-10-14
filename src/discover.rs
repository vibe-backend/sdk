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
        if !name.ends_with(".md") || name.ends_with(".lock.md") {
            continue;
        }
        if name == "schema.md" {
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
        let schema_path = file.abs.parent().map(|parent| parent.join("schema.md"));
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
    let classified = match parts.as_slice() {
        ["providers", file] => simple_file(KindHint::NoneScope, file, "providers")?,
        ["pipelines", file] => simple_file(KindHint::PipelineWrap, file, "pipelines")?,
        ["migrations", provider, file] => classify_migration(provider, file)?,
        ["models", object, file] => classify_model(object, file)?,
        ["models", object, "methods", file] => classify_method(object, file)?,
        ["api", definition, file] => classify_definition(definition, file)?,
        ["api", definition, version, file] => classify_version(definition, version, file)?,
        ["api", definition, version, feature, file] => {
            classify_feature(definition, version, feature, file)?
        }
        _ => {
            return Err(Diag::error(
                &rel,
                "path does not match providers, pipelines, migrations, models, or api/{definition}/{version}/{feature}",
            ));
        }
    };
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
    if file != "_version.md" {
        return Err(Diag::error(
            &rel,
            "only _version.md is allowed directly under an api version",
        ));
    }
    Ok(Classified {
        kind: Kind::Api,
        scope: Some(Scope::Version),
        id: format!("{definition}/{version}/_version"),
    })
}

fn classify_feature(
    definition: &str,
    version: &str,
    feature: &str,
    file: &str,
) -> Result<Classified, Diag> {
    let rel = format!("api/{definition}/{version}/{feature}/{file}");
    check_kebab(definition).map_err(|message| Diag::error(&rel, message))?;
    check_version(version).map_err(|message| Diag::error(&rel, message))?;
    check_kebab(feature).map_err(|message| Diag::error(&rel, message))?;
    let stem = stem_md(file).map_err(|message| Diag::error(&rel, message))?;
    if stem != feature {
        return Err(Diag::error(
            &rel,
            "feature file name must match the folder name",
        ));
    }
    Ok(Classified {
        kind: Kind::Api,
        scope: Some(Scope::Feature),
        id: format!("{definition}/{version}/{feature}"),
    })
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
