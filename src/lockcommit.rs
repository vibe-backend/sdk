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

fn write_journal(path: &Path, journal: &Journal) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(journal).map_err(std::io::Error::other)?;
    write_synced(&tmp, &bytes, false)?;
    fs::rename(&tmp, path)?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))
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
    let path = journal_path(root);
    let text = match fs::read(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let journal: Journal = serde_json::from_slice(&text).map_err(std::io::Error::other)?;
    if journal.committed {
        finish(&journal.entries)?;
    } else {
        roll_back(&journal.entries)?;
    }
    fs::remove_file(&path)?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}

/// Replace every file in one step, without a journal (cache writes).
pub fn commit(writes: &[(PathBuf, String)]) -> std::io::Result<()> {
    swap(None, writes)
}

/// Replace every lock in one step, journaled under `root/.vibe`.
pub fn commit_journaled(root: &Path, writes: &[(PathBuf, String)]) -> std::io::Result<()> {
    swap(Some(&journal_path(root)), writes)
}

fn swap(journal_file: Option<&Path>, writes: &[(PathBuf, String)]) -> std::io::Result<()> {
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
        let bak = root.join("models/a.lock.md.bak-x");
        fs::write(&bak, "old-a").unwrap();
        write_journal(
            &journal_path(&root),
            &Journal {
                committed: true,
                entries: vec![Entry {
                    path: a.clone(),
                    tmp: root.join("models/a.lock.md.tmp-x"),
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
