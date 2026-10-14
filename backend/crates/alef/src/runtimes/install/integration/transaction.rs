// SPDX-License-Identifier: MIT OR Apache-2.0
//! Same-volume artifact replacement with rollback backups kept outside launcher suffixes.
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn backup_path(root: &Path) -> io::Result<PathBuf> {
    loop {
        let candidate = root.join(format!(
            ".alef-integration-backup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Moves existing same-identity artifacts aside before publishing stage.
/// `validate` checks owned artifacts without following links; `commit` must be the last fallible operation.
pub(crate) fn publish<F, C>(
    root: &Path,
    destination: &Path,
    stage: &Path,
    stale: &[PathBuf],
    validate: F,
    commit: C,
) -> io::Result<()>
where
    F: Fn(&Path) -> io::Result<()>,
    C: FnOnce() -> io::Result<()>,
{
    publish_with_step(
        root,
        destination,
        stage,
        stale,
        validate,
        commit,
        |_| Ok(()),
    )
}

fn publish_with_step<F, C, H>(
    root: &Path,
    destination: &Path,
    stage: &Path,
    stale: &[PathBuf],
    validate: F,
    commit: C,
    mut step: H,
) -> io::Result<()>
where
    F: Fn(&Path) -> io::Result<()>,
    C: FnOnce() -> io::Result<()>,
    H: FnMut(usize) -> io::Result<()>,
{
    if !crate::runtimes::store::is_plain_directory(root)
        || destination.parent() != Some(root)
        || stage.parent() != Some(root)
    {
        return Err(with_rollback(
            io::Error::new(
                io::ErrorKind::InvalidData,
                "unsafe integration root or path",
            ),
            remove_artifact(stage),
        ));
    }
    let stage_meta = match std::fs::symlink_metadata(stage) {
        Ok(meta) => meta,
        Err(error) => return Err(error),
    };
    if !(crate::runtimes::store::is_plain_directory(stage)
        || crate::runtimes::store::is_regular_file(stage))
        || stage_meta.file_type().is_symlink()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsafe integration stage",
        ));
    }
    if let Err(error) = validate(stage) {
        return Err(with_rollback(error, remove_artifact(stage)));
    }
    let mut candidates = stale.to_vec();
    candidates.push(destination.to_path_buf());
    let mut old: Vec<(PathBuf, PathBuf)> = Vec::new();
    for path in &candidates {
        if old.iter().any(|(prior, _)| prior == path) {
            continue;
        }
        if path.parent() != Some(root) {
            return Err(with_rollback(
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "artifact outside integration root",
                ),
                remove_artifact(stage),
            ));
        }
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                if let Err(error) = validate(path) {
                    return Err(with_rollback(error, remove_artifact(stage)));
                }
                old.push((
                    path.to_path_buf(),
                    match backup_path(root) {
                        Ok(path) => path,
                        Err(error) => return Err(with_rollback(error, remove_artifact(stage))),
                    },
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(with_rollback(error, remove_artifact(stage))),
        }
    }
    let mut moved = 0usize;
    for (path, backup) in &old {
        if let Err(error) = std::fs::rename(path, backup) {
            let rollback = restore(&old[..moved]);
            return Err(with_rollback(
                error,
                combine(rollback, remove_artifact(stage)),
            ));
        }
        moved += 1;
        if let Err(error) = step(moved) {
            let rollback = restore(&old[..moved]);
            return Err(with_rollback(
                error,
                combine(rollback, remove_artifact(stage)),
            ));
        }
    }
    if let Err(error) = std::fs::rename(stage, destination) {
        let rollback = restore(&old[..moved]);
        return Err(with_rollback(
            error,
            combine(rollback, remove_artifact(stage)),
        ));
    }
    if let Err(error) = step(moved + 1) {
        let rollback = combine(remove_artifact(destination), restore(&old[..moved]));
        return Err(with_rollback(
            error,
            combine(rollback, remove_artifact(stage)),
        ));
    }
    if let Err(error) = commit() {
        let rollback = combine(remove_artifact(destination), restore(&old[..moved]));
        return Err(with_rollback(
            error,
            combine(rollback, remove_artifact(stage)),
        ));
    }
    // No fallible operation after commit can turn a committed installation into failure.
    for (_, backup) in old {
        if let Err(error) = remove_artifact(&backup) {
            eprintln!(
                "alef: APP_STORE: installed artifact; backup cleanup failed at {}: {error}",
                backup.display()
            );
        }
    }
    Ok(())
}

fn restore(old: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    let mut errors = Vec::new();
    for (path, backup) in old.iter().rev() {
        if let Err(error) = std::fs::rename(backup, path) {
            errors.push(format!("{}: {error}", path.display()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(errors.join("; ")))
    }
}
fn combine(first: io::Result<()>, second: io::Result<()>) -> io::Result<()> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(a), Ok(())) | (Ok(()), Err(a)) => Err(a),
        (Err(a), Err(b)) => Err(io::Error::other(format!("{a}; {b}"))),
    }
}
fn with_rollback(error: io::Error, rollback: io::Result<()>) -> io::Error {
    match rollback {
        Ok(()) => error,
        Err(rollback) => io::Error::other(format!("{error}; rollback failed: {rollback}")),
    }
}
pub(crate) fn remove_artifact(path: &Path) -> io::Result<()> {
    fn validate(path: &Path, depth: usize) -> io::Result<()> {
        if depth > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact nesting too deep",
            ));
        }
        let meta = std::fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink()
            || !(crate::runtimes::store::is_regular_file(path)
                || crate::runtimes::store::is_plain_directory(path))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact contains linked or special entry",
            ));
        }
        if crate::runtimes::store::is_plain_directory(path) {
            for entry in std::fs::read_dir(path)? {
                validate(&entry?.path(), depth + 1)?;
            }
        }
        Ok(())
    }
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => {
            validate(path, 0)?;
            if crate::runtimes::store::is_plain_directory(path) {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SCRATCH: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "alef-transaction-{}-{}",
                    std::process::id(),
                    SCRATCH.fetch_add(1, Ordering::Relaxed)
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(std::fs::canonicalize(path).unwrap()),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("{error}"),
                }
            }
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tree(path: &Path, content: &[u8]) {
        std::fs::create_dir(path).unwrap();
        std::fs::write(path.join("marker"), content).unwrap();
    }
    fn assert_restored(root: &Path, destination: &Path, stage: &Path, old: &[(&Path, &[u8])]) {
        for (path, expected) in old {
            assert_eq!(std::fs::read(path.join("marker")).unwrap(), *expected);
        }
        assert_eq!(
            std::fs::read(destination.join("marker")).unwrap(),
            b"destination"
        );
        assert!(!stage.exists());
        assert!(std::fs::read_dir(root).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".alef-integration-backup-")));
    }
    #[test]
    fn commit_failure_restores_destination_and_stale_artifacts_and_cleans_stage() {
        let scratch = Scratch::new();
        let root = &scratch.0;
        let destination = root.join("new.app");
        let old_a = root.join("old-a.app");
        let old_b = root.join("old-b.app");
        tree(&destination, b"destination");
        tree(&old_a, b"old-a");
        tree(&old_b, b"old-b");
        let stage = root.join("stage");
        tree(&stage, b"new");
        let result = publish(
            root,
            &destination,
            &stage,
            &[old_a.clone(), old_b.clone()],
            |_| Ok(()),
            || Err(io::Error::other("injected commit failure")),
        );
        assert!(result.is_err());
        assert_restored(
            root,
            &destination,
            &stage,
            &[(&old_a, b"old-a"), (&old_b, b"old-b")],
        );
    }
    #[test]
    fn each_publish_step_failure_restores_every_artifact() {
        for failure in 1..=4 {
            let scratch = Scratch::new();
            let root = &scratch.0;
            let destination = root.join("new.app");
            let a = root.join("a.app");
            let b = root.join("b.app");
            tree(&destination, b"destination");
            tree(&a, b"a");
            tree(&b, b"b");
            let stage = root.join("stage");
            tree(&stage, b"new");
            let result = publish_with_step(
                root,
                &destination,
                &stage,
                &[a.clone(), b.clone()],
                |_| Ok(()),
                || Ok(()),
                |index| {
                    if index == failure {
                        Err(io::Error::other("injected publish failure"))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err(), "step {failure}");
            assert_restored(root, &destination, &stage, &[(&a, b"a"), (&b, b"b")]);
        }
    }
}
