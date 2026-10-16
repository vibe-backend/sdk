//! All-or-nothing lock writes with a journal (#83).
//!
//! `commit_journaled` stages every file to a fsynced temp file, records the
//! plan in `.vibe/lock-journal.json`, moves old files to backups, renames the
//! temps in, fsyncs the directories, marks the journal committed, and then
//! removes the backups and the journal. If the process dies part way, the
//! next run calls `recover`: an uncommitted journal is rolled back to the old
//! files, a committed one is finished; temp and backup files are removed.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Entry {
    path: PathBuf,
    tmp: PathBuf,
    /// Backup of the previous file, when there was one.
    bak: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Journal {
    committed: bool,
    entries: Vec<Entry>,
}

pub fn journal_path(root: &Path) -> PathBuf {
    root.join(".vibe").join("lock-journal.json")
}

// Test hook: fail (or "crash", leaving everything as is) at the n-th
// rename of the swap phase.
#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_AT: std::cell::Cell<Option<(usize, bool)>> = const { std::cell::Cell::new(None) };
}

fn injected(step: usize) -> Option<(std::io::Error, bool)> {
    #[cfg(test)]
    {
        if let Some((n, crash)) = FAIL_AT.with(|f| f.get()) {
            if n == step {
                return Some((std::io::Error::other("injected rename failure"), crash));
            }
        }
    }
    let _ = step;
    None
}

fn suffix(path: &Path, kind: &str, tag: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{kind}-{tag}"));
    path.with_file_name(name)
}

fn write_synced(path: &Path, bytes: &[u8], create_new: bool) -> std::io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true);
    if create_new {
        opts.create_new(true);
    } else {
        opts.create(true).truncate(true);
    }
    let mut file = opts.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

fn sync_parents<'a>(paths: impl Iterator<Item = &'a Path>) -> std::io::Result<()> {
    let mut dirs: Vec<&Path> = paths.filter_map(Path::parent).collect();
    dirs.sort();
    dirs.dedup();
    for dir in dirs {
        sync_dir(dir)?;
    }
    Ok(())
}

fn invalid(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.into())
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// The `.vibe` directory as a real directory (created if missing, never a
/// symlink).
pub fn ensure_state_dir(dir: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => Err(invalid(format!(
            "{} is a symlink; refusing to use it",
            dir.display()
        ))),
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(invalid(format!("{} is not a directory", dir.display()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                if is_symlink(parent) {
                    return Err(invalid(format!("{} is a symlink", parent.display())));
                }
            }
            fs::create_dir(dir)
        }
        Err(err) => Err(err),
    }
}

/// Write the journal through a fresh, randomly named temp file opened with
/// `create_new` (never follows or reuses an existing path), then rename it
/// over the journal (rename replaces a symlink, it does not follow it).
fn write_journal(path: &Path, journal: &Journal) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    ensure_state_dir(dir)?;
    let tmp = dir.join(format!(
        ".lock-journal.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let bytes = serde_json::to_vec_pretty(journal).map_err(std::io::Error::other)?;
    if let Err(err) = write_synced(&tmp, &bytes, true) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    sync_dir(dir)
}

/// `<name>.<kind>-<32 lowercase hex>` → the tag, when `file` is exactly the
/// temp/backup name `swap` gives `path`.
fn tag_of(path: &Path, file: &Path, kind: &str) -> Option<String> {
    if file.parent() != path.parent() {
        return None;
    }
    let base = path.file_name()?.to_str()?;
    let name = file.file_name()?.to_str()?;
    let tag = name
        .strip_prefix(base)?
        .strip_prefix(&format!(".{kind}-"))?;
    (tag.len() == 32
        && tag
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then(|| tag.to_string())
}

/// Every journal path must be a `*.lock.md` file strictly inside `root`,
/// reached without symlinks, with tmp/bak named exactly as `swap` names
/// them. Anything else means the journal was not written by us: refuse it
/// and touch nothing.
fn check_entries(root: &Path, entries: &[Entry]) -> std::io::Result<()> {
    let root_real = fs::canonicalize(root)?;
    for (idx, entry) in entries.iter().enumerate() {
        let bad = |why: &str| {
            invalid(format!(
                "lock journal entry {}: {why}; not recovering",
                idx + 1
            ))
        };
        let path = &entry.path;
        let rel = path
            .strip_prefix(root)
            .or_else(|_| path.strip_prefix(&root_real));
        let Ok(rel) = rel else {
            return Err(bad("path is outside the project"));
        };
        if !rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
            || rel.components().next().is_none()
        {
            return Err(bad("path is not a plain relative path"));
        }
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".lock.md"))
        {
            return Err(bad("path is not a lock file"));
        }
        if rel.starts_with(".vibe") || rel.starts_with(".git") {
            return Err(bad("path is in a reserved directory"));
        }
        // No symlinked directory between the root and the file.
        let parent = path.parent().ok_or_else(|| bad("path has no parent"))?;
        match fs::canonicalize(parent) {
            Ok(real) => {
                let want = root_real.join(rel.parent().unwrap_or(Path::new("")));
                if real != want {
                    return Err(bad("path goes through a symlink"));
                }
            }
            Err(_) => return Err(bad("parent directory is missing")),
        }
        let Some(tag) = tag_of(path, &entry.tmp, "tmp") else {
            return Err(bad("unexpected temp file name"));
        };
        if let Some(bak) = &entry.bak {
            if tag_of(path, bak, "bak").as_deref() != Some(tag.as_str()) {
                return Err(bad("unexpected backup file name"));
            }
        }
        for file in [Some(path), Some(&entry.tmp), entry.bak.as_ref()]
            .into_iter()
            .flatten()
        {
            if is_symlink(file) {
                return Err(bad("a journaled file is a symlink"));
            }
        }
    }
    Ok(())
}

/// Remove `*.lock.md.tmp-<uuid>` files left by a crash before the journal
/// was written (no journal refers to them). Symlinks are never followed.
fn sweep_orphan_temps(root: &Path) -> std::io::Result<()> {
    fn walk(dir: &Path, depth: usize) -> std::io::Result<()> {
        if depth > 16 {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if name == ".git" || name == ".vibe" || name == "target" || name == "node_modules" {
                    continue;
                }
                walk(&entry.path(), depth + 1)?;
            } else if kind.is_file() {
                if let Some((base, _)) = name.rsplit_once(".tmp-") {
                    let path = entry.path();
                    if base.ends_with(".lock.md")
                        && tag_of(&path.with_file_name(base), &path, "tmp").is_some()
                    {
                        remove_if_exists(&path)?;
                    }
                }
            }
        }
        Ok(())
    }
    walk(root, 0)
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Put every entry back to its previous state (idempotent).
fn roll_back(entries: &[Entry]) -> std::io::Result<()> {
    for entry in entries.iter().rev() {
        match &entry.bak {
            Some(bak) if bak.exists() => fs::rename(bak, &entry.path)?,
            Some(_) => {} // never moved aside: the old file is still in place
            None => {
                // No previous file. Remove ours only if it was renamed in.
                if !entry.tmp.exists() {
                    remove_if_exists(&entry.path)?;
                }
            }
        }
        remove_if_exists(&entry.tmp)?;
    }
    sync_parents(entries.iter().map(|e| e.path.as_path()))
}

fn finish(entries: &[Entry]) -> std::io::Result<()> {
    for entry in entries {
        if let Some(bak) = &entry.bak {
            remove_if_exists(bak)?;
        }
        remove_if_exists(&entry.tmp)?;
    }
    Ok(())
}

/// Repair an interrupted commit left by a previous run.
pub fn recover(root: &Path) -> std::io::Result<()> {
    let state = root.join(".vibe");
    if is_symlink(&state) {
        return Err(invalid(format!(
            "{} is a symlink; refusing to use it",
            state.display()
        )));
    }
    let path = journal_path(root);
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_file() => {
            return Err(invalid(
                "lock journal is not a regular file; not recovering",
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return sweep_orphan_temps(root);
        }
        Err(err) => return Err(err),
    }
    let text = fs::read(&path)?;
    let journal: Journal = serde_json::from_slice(&text).map_err(std::io::Error::other)?;
    check_entries(root, &journal.entries)?;
    if journal.committed {
        finish(&journal.entries)?;
    } else {
        roll_back(&journal.entries)?;
    }
    fs::remove_file(&path)?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}

/// One compile run at a time per project: `.vibe/compile.lock`, created
/// with `create_new` and removed on drop. A crashed run leaves it behind;
/// the error says how to clear it.
pub struct RunLock {
    path: PathBuf,
}

impl RunLock {
    pub fn acquire(root: &Path) -> std::io::Result<Self> {
        let dir = root.join(".vibe");
        ensure_state_dir(&dir)?;
        let path = dir.join("compile.lock");
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                Ok(Self { path })
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Err(std::io::Error::new(
                err.kind(),
                format!(
                    "another vibe compile is running in this project ({} exists; delete it if no compile is running)",
                    path.display()
                ),
            )),
            Err(err) => Err(err),
        }
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Replace every file in one step, without a journal (cache writes).
pub fn commit(writes: &[(PathBuf, String)]) -> std::io::Result<()> {
    swap(None, writes)
}

/// A failed commit, and whether it failed after the commit point (the new
/// files are in place; only cleanup is left for the next run).
#[derive(Debug)]
pub struct CommitError {
    pub committed: bool,
    pub source: std::io::Error,
}

impl std::fmt::Display for CommitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}

/// Replace every lock in one step, journaled under `root/.vibe`.
pub fn commit_journaled(root: &Path, writes: &[(PathBuf, String)]) -> Result<(), CommitError> {
    let mut committed = false;
    swap_inner(Some(&journal_path(root)), writes, &mut committed)
        .map_err(|source| CommitError { committed, source })
}

fn swap(journal_file: Option<&Path>, writes: &[(PathBuf, String)]) -> std::io::Result<()> {
    let mut committed = false;
    swap_inner(journal_file, writes, &mut committed)
}

fn swap_inner(
    journal_file: Option<&Path>,
    writes: &[(PathBuf, String)],
    committed: &mut bool,
) -> std::io::Result<()> {
    if writes.is_empty() {
        return Ok(());
    }
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let mut entries: Vec<Entry> = Vec::new();
    for (path, text) in writes {
        let tmp = suffix(path, "tmp", &tag);
        if let Err(err) = write_synced(&tmp, text.as_bytes(), true) {
            let _ = remove_if_exists(&tmp);
            for e in &entries {
                let _ = remove_if_exists(&e.tmp);
            }
            return Err(err);
        }
        let bak = path.exists().then(|| suffix(path, "bak", &tag));
        entries.push(Entry {
            path: path.clone(),
            tmp,
            bak,
        });
    }
    let mut journal = Journal {
        committed: false,
        entries,
    };
    if let Some(jf) = journal_file {
        if let Err(err) = write_journal(jf, &journal) {
            let _ = roll_back(&journal.entries);
            return Err(err);
        }
    }
    let fail = |journal: &Journal, err: std::io::Error| -> std::io::Error {
        match roll_back(&journal.entries) {
            Ok(()) => {
                if let Some(jf) = journal_file {
                    let _ = fs::remove_file(jf);
                }
                err
            }
            // Leave the journal so the next run can finish the rollback.
            Err(again) => std::io::Error::other(format!("{err}; rollback failed: {again}")),
        }
    };
    let mut step = 0usize;
    for entry in &journal.entries {
        for (from, to) in [
            entry.bak.as_ref().map(|bak| (&entry.path, bak)),
            Some((&entry.tmp, &entry.path)),
        ]
        .into_iter()
        .flatten()
        {
            step += 1;
            if let Some((err, crash)) = injected(step) {
                if crash {
                    return Err(err);
                }
                return Err(fail(&journal, err));
            }
            if let Err(err) = fs::rename(from, to) {
                return Err(fail(&journal, err));
            }
        }
    }
    if let Err(err) = sync_parents(journal.entries.iter().map(|e| e.path.as_path())) {
        return Err(fail(&journal, err));
    }
    journal.committed = true;
    if let Some(jf) = journal_file {
        if let Err(err) = write_journal(jf, &journal) {
            return Err(fail(&journal, err));
        }
    }
    // Commit point: the new files are in place from here on.
    *committed = true;
    finish(&journal.entries)?;
    if let Some(jf) = journal_file {
        fs::remove_file(jf)?;
        sync_dir(jf.parent().unwrap_or(Path::new(".")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG: &str = "0123456789abcdef0123456789abcdef";

    fn plant(root: &Path, entries: Vec<Entry>) {
        let dir = root.join(".vibe");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            journal_path(root),
            serde_json::to_vec(&Journal {
                committed: false,
                entries,
            })
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn malicious_journal_paths_are_refused_and_nothing_is_touched() {
        let (root, a, _) = setup();
        let outside_dir =
            std::env::temp_dir().join(format!("vibe-outside-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&outside_dir).unwrap();
        let victim = outside_dir.join("victim.lock.md");
        fs::write(&victim, "keep").unwrap();
        let in_root = root.join("models/notes.txt");
        fs::write(&in_root, "keep").unwrap();
        let tmp = |p: &Path| suffix(p, "tmp", TAG);
        let bak = |p: &Path| suffix(p, "bak", TAG);
        let cases: Vec<Entry> = vec![
            // Outside the project (absolute).
            Entry {
                path: victim.clone(),
                tmp: tmp(&victim),
                bak: None,
            },
            // Escapes with `..`.
            Entry {
                path: root.join("models/../../").join(victim.file_name().unwrap()),
                tmp: tmp(&victim),
                bak: None,
            },
            // Not a lock file.
            Entry {
                path: in_root.clone(),
                tmp: tmp(&in_root),
                bak: None,
            },
            // bak points at another file (would be renamed over `path`).
            Entry {
                path: a.clone(),
                tmp: tmp(&a),
                bak: Some(victim.clone()),
            },
            // tmp/bak with a wrong name or tag.
            Entry {
                path: a.clone(),
                tmp: a.with_file_name("a.lock.md.tmp-x"),
                bak: None,
            },
            Entry {
                path: a.clone(),
                tmp: tmp(&a),
                bak: Some(suffix(&a, "bak", "fedcba9876543210fedcba9876543210")),
            },
            Entry {
                path: a.clone(),
                tmp: tmp(&a),
                bak: Some(tmp(&a)),
            },
        ];
        for entry in cases {
            let shown = format!("{entry:?}");
            plant(&root, vec![entry]);
            let err = recover(&root).unwrap_err();
            assert!(err.to_string().contains("not recovering"), "{shown}: {err}");
            assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
            assert_eq!(fs::read_to_string(&in_root).unwrap(), "keep");
            assert_eq!(fs::read_to_string(&a).unwrap(), "old-a");
            assert!(journal_path(&root).exists(), "refused journal is kept");
        }
        let _ = bak;
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed_on_recovery() {
        use std::os::unix::fs::symlink;
        let (root, _, _) = setup();
        let outside = std::env::temp_dir().join(format!("vibe-outside-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("v.lock.md");
        fs::write(&victim, "keep").unwrap();
        // A symlinked directory inside the project pointing outside.
        symlink(&outside, root.join("models/link")).unwrap();
        let path = root.join("models/link/v.lock.md");
        plant(
            &root,
            vec![Entry {
                path: path.clone(),
                tmp: suffix(&path, "tmp", TAG),
                bak: None,
            }],
        );
        assert!(recover(&root).is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        // The journaled lock path itself is a symlink.
        fs::remove_file(root.join("models/link")).unwrap();
        let lock = root.join("models/s.lock.md");
        symlink(&victim, &lock).unwrap();
        plant(
            &root,
            vec![Entry {
                path: lock.clone(),
                tmp: suffix(&lock, "tmp", TAG),
                bak: None,
            }],
        );
        assert!(recover(&root).is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_state_dir_is_refused() {
        use std::os::unix::fs::symlink;
        let (root, a, b) = setup();
        let outside = std::env::temp_dir().join(format!("vibe-outside-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join(".vibe")).unwrap();
        assert!(recover(&root).is_err());
        let err = commit_journaled(&root, &writes(&a, &b, &root)).unwrap_err();
        assert!(!err.committed);
        assert!(err.to_string().contains("symlink"), "{err}");
        assert_eq!(
            fs::read_dir(&outside).unwrap().count(),
            0,
            "nothing written outside"
        );
        assert_eq!(fs::read_to_string(&a).unwrap(), "old-a");
        assert_eq!(listing(&root.join("models")), ["a.lock.md", "b.lock.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn journal_write_does_not_follow_a_planted_symlink() {
        use std::os::unix::fs::symlink;
        let (root, a, b) = setup();
        let outside = std::env::temp_dir().join(format!("vibe-outside-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("victim");
        fs::write(&victim, "keep").unwrap();
        fs::create_dir_all(root.join(".vibe")).unwrap();
        // The old fixed temp name, and the journal itself, as symlinks.
        symlink(&victim, root.join(".vibe/lock-journal.json.tmp")).unwrap();
        symlink(&victim, journal_path(&root)).unwrap();
        FAIL_AT.with(|f| f.set(Some((1, true))));
        let _ = commit_journaled(&root, &writes(&a, &b, &root));
        FAIL_AT.with(|f| f.set(None));
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        // The journal was replaced by a regular file, not written through.
        assert!(!is_symlink(&journal_path(&root)));
        recover(&root).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "old-a");
    }

    #[test]
    fn a_second_run_cannot_take_the_run_lock() {
        let (root, _, _) = setup();
        let first = RunLock::acquire(&root).unwrap();
        let err = RunLock::acquire(&root).err().expect("second run must fail");
        assert!(err.to_string().contains("another vibe compile"), "{err}");
        drop(first);
        RunLock::acquire(&root).unwrap();
    }

    #[test]
    fn temps_left_before_the_journal_are_swept() {
        let (root, a, _) = setup();
        let orphan = suffix(&a, "tmp", TAG);
        fs::write(&orphan, "half").unwrap();
        let other = root.join("models/keep.md.tmp-x");
        fs::write(&other, "keep").unwrap();
        recover(&root).unwrap();
        assert!(!orphan.exists());
        assert!(other.exists());
    }

    #[test]
    fn failure_after_the_commit_point_is_reported_as_committed() {
        let (root, a, b) = setup();
        FAIL_AT.with(|f| f.set(Some((1, false))));
        let err = commit_journaled(&root, &writes(&a, &b, &root)).unwrap_err();
        FAIL_AT.with(|f| f.set(None));
        assert!(!err.committed);
        commit_journaled(&root, &writes(&a, &b, &root)).unwrap();
    }

    fn setup() -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("vibe-journal-{}", uuid::Uuid::new_v4()));
        let dir = root.join("models");
        fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.lock.md");
        fs::write(&a, "old-a").unwrap();
        let b = dir.join("b.lock.md");
        fs::write(&b, "old-b").unwrap();
        (root, a, b)
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    fn writes(a: &Path, b: &Path, root: &Path) -> Vec<(PathBuf, String)> {
        vec![
            (a.to_path_buf(), "new-a".into()),
            (b.to_path_buf(), "new-b".into()),
            (root.join("models/c.lock.md"), "new-c".into()),
        ]
    }

    #[test]
    fn rename_failure_rolls_back_every_file() {
        // Steps: a->bak, tmp->a, b->bak, tmp->b, tmp->c. Fail at each.
        for step in 1..=5 {
            let (root, a, b) = setup();
            FAIL_AT.with(|f| f.set(Some((step, false))));
            let err = commit_journaled(&root, &writes(&a, &b, &root));
            FAIL_AT.with(|f| f.set(None));
            assert!(err.is_err(), "step {step}");
            assert_eq!(fs::read_to_string(&a).unwrap(), "old-a", "step {step}");
            assert_eq!(fs::read_to_string(&b).unwrap(), "old-b", "step {step}");
            assert_eq!(listing(&root.join("models")), ["a.lock.md", "b.lock.md"]);
            assert!(!journal_path(&root).exists());
        }
    }

    #[test]
    fn crash_mid_swap_is_repaired_on_the_next_run() {
        for step in 1..=5 {
            let (root, a, b) = setup();
            FAIL_AT.with(|f| f.set(Some((step, true))));
            let _ = commit_journaled(&root, &writes(&a, &b, &root));
            FAIL_AT.with(|f| f.set(None));
            assert!(journal_path(&root).exists(), "journal left by the crash");
            recover(&root).unwrap();
            assert_eq!(fs::read_to_string(&a).unwrap(), "old-a", "step {step}");
            assert_eq!(fs::read_to_string(&b).unwrap(), "old-b", "step {step}");
            assert_eq!(listing(&root.join("models")), ["a.lock.md", "b.lock.md"]);
            assert!(!journal_path(&root).exists());
        }
    }

    #[test]
    fn committed_journal_is_finished_on_recovery() {
        let (root, a, b) = setup();
        commit_journaled(&root, &writes(&a, &b, &root)).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "new-a");
        assert_eq!(
            listing(&root.join("models")),
            ["a.lock.md", "b.lock.md", "c.lock.md"]
        );
        // A crash after the commit point leaves backups and a committed journal.
        let bak = root.join(format!("models/a.lock.md.bak-{TAG}"));
        fs::write(&bak, "old-a").unwrap();
        write_journal(
            &journal_path(&root),
            &Journal {
                committed: true,
                entries: vec![Entry {
                    path: a.clone(),
                    tmp: root.join(format!("models/a.lock.md.tmp-{TAG}")),
                    bak: Some(bak.clone()),
                }],
            },
        )
        .unwrap();
        recover(&root).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "new-a");
        assert!(!bak.exists());
        assert!(!journal_path(&root).exists());
    }
}
