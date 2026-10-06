// SPDX-License-Identifier: MIT OR Apache-2.0
//! Files dropped on a window: the document of the window may read them, and is told
//! (`window.file-drop`).
use std::path::PathBuf;

use alef_core::session::SessionManager;
use serde_json::json;

use super::events::FILE_DROP;
use crate::{ui::event_json, window::App};

/// The most paths one drop is taken to bring; the rest is left out.
pub(in crate::window) const MAX_DROPPED: usize = 500;

/// Gives the document of `window` read access to what was dropped (a folder with everything below
/// it) and returns the paths that were given, in the order dropped. A path that is not Unicode, not
/// absolute or not there any more is left out, and so is a drop on a window without a document.
fn granted(sessions: &SessionManager, window: u64, paths: Vec<PathBuf>) -> Vec<String> {
    let Some(session) = sessions.current(window) else {
        return Vec::new();
    };
    let grants = session.grants();
    paths
        .into_iter()
        .filter_map(|path| {
            let text = path.to_str()?.to_owned();
            // What was dropped is there; a name that is not would be granted as a file to come.
            std::fs::metadata(&path).ok()?;
            grants.grant_read(&path).ok().map(|()| text)
        })
        .collect()
}

impl App {
    /// Takes a drop of `paths` on the window `window_id`: the grant first, then the event.
    pub(in crate::window) fn file_drop(&mut self, window_id: u64, paths: Vec<PathBuf>) {
        let Some(state) = self
            .windows
            .iter()
            .find(|state| state.window_id == window_id)
        else {
            return;
        };
        let label = state.label.clone();
        let paths = granted(&self.sessions, window_id, paths);
        if paths.is_empty() {
            return;
        }
        if let Ok(event) = event_json(FILE_DROP, &json!({ "label": label, "paths": paths })) {
            self.handle.events().publish(Some(window_id), &event);
        }
    }

    /// Hands over what the windows have been dropped since the last pass, one event per window:
    /// the files of one drop arrive one by one and are told together.
    pub(in crate::window) fn deliver_drops(&mut self) {
        let drops: Vec<(u64, Vec<PathBuf>)> = self
            .windows
            .iter_mut()
            .filter(|state| !state.dropped.is_empty())
            .map(|state| (state.window_id, std::mem::take(&mut state.dropped)))
            .collect();
        for (window, paths) in drops {
            self.file_drop(window, paths);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, sync::Arc};

    use alef_core::{
        protocol::call::Limits,
        security::{
            manifest::Permissions,
            permissions::{PathVars, PermissionSet},
        },
    };
    use serde_json::json;

    use super::*;

    fn permissions(root: &Path) -> PermissionSet {
        let vars = PathVars {
            app_data: root.join("data"),
            app_config: root.join("config"),
            app_cache: root.join("cache"),
            home: root.join("home"),
            documents: root.join("documents"),
            downloads: root.join("downloads"),
            desktop: root.join("desktop"),
            temp: root.join("temp"),
            app: root.join("app"),
        };
        let policy: Permissions = serde_json::from_value(json!({
            "fs": {"read": [], "write": []}, "cli": {"exec": []},
            "net": {"http": [], "socket": []}, "shell": {"openExternal": []},
            "clipboard": {"read": false}, "shortcut": {"global": false},
            "secrets": false, "app": {"env": []}
        }))
        .expect("permissions");
        PermissionSet::from_manifest(&policy, &vars).expect("permission set")
    }

    async fn sessions() -> SessionManager {
        SessionManager::new(Arc::new(|| "token".to_owned()), Limits::default())
    }

    fn may_read(
        permissions: &PermissionSet,
        sessions: &SessionManager,
        window: u64,
        path: &Path,
    ) -> bool {
        let session = sessions.current(window).expect("a session");
        permissions
            .check(
                alef_core::security::permissions::Permission::FsRead,
                path.to_str(),
                &session.grants(),
            )
            .is_ok()
    }

    #[tokio::test]
    async fn dropped_files_and_folders_become_readable_for_the_document_of_that_window_only() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("a.txt");
        let sibling = directory.path().join("b.txt");
        let folder = directory.path().join("folder");
        let inside = folder.join("deep").join("c.txt");
        fs::create_dir_all(inside.parent().unwrap()).unwrap();
        for path in [&file, &sibling, &inside] {
            fs::write(path, "x").unwrap();
        }
        let permissions = permissions(directory.path());
        let sessions = sessions().await;
        sessions.begin_document(1).await;
        sessions.begin_document(2).await;
        let told = granted(&sessions, 1, vec![file.clone(), folder.clone()]);
        assert_eq!(
            told,
            [
                file.to_string_lossy().into_owned(),
                folder.to_string_lossy().into_owned()
            ]
        );
        assert!(may_read(&permissions, &sessions, 1, &file));
        assert!(
            may_read(&permissions, &sessions, 1, &inside),
            "a folder with everything below it"
        );
        assert!(
            !may_read(&permissions, &sessions, 1, &sibling),
            "what was not dropped stays out of reach"
        );
        assert!(
            !may_read(&permissions, &sessions, 2, &file),
            "another window is given nothing"
        );
    }

    #[tokio::test]
    async fn what_cannot_be_granted_is_left_out_of_the_event() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("a.txt");
        fs::write(&file, "x").unwrap();
        let sessions = sessions().await;
        sessions.begin_document(1).await;
        let told = granted(
            &sessions,
            1,
            vec![
                directory.path().join("never-was.txt"),
                PathBuf::from("relative/path.txt"),
                file.clone(),
            ],
        );
        assert_eq!(told, [file.to_string_lossy().into_owned()]);
    }

    #[tokio::test]
    async fn a_window_without_a_document_grants_nothing_and_tells_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("a.txt");
        fs::write(&file, "x").unwrap();
        let sessions = sessions().await;
        assert!(granted(&sessions, 7, vec![file]).is_empty());
    }

    #[tokio::test]
    async fn the_grants_of_a_document_that_is_gone_are_gone_with_it() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("a.txt");
        fs::write(&file, "x").unwrap();
        let permissions = permissions(directory.path());
        let sessions = sessions().await;
        sessions.begin_document(1).await;
        granted(&sessions, 1, vec![file.clone()]);
        assert!(may_read(&permissions, &sessions, 1, &file));
        sessions.begin_document(1).await;
        assert!(
            !may_read(&permissions, &sessions, 1, &file),
            "the document that loaded again was given nothing"
        );
    }
}
