// SPDX-License-Identifier: MIT OR Apache-2.0
//! `dialog.*` on the UI thread: a native dialog on top of the window of the caller. The UI thread
//! only prepares the dialog; it is shown and awaited on the Tokio runtime, one at a time, and the
//! answer goes to the document from there, so the windows keep painting meanwhile.
//!
//! An end-to-end run (`ALEF_E2E=1` with `ALEF_E2E_DIALOGS`) answers from a script instead and
//! shows nothing: a test must not put a dialog in front of anybody.
use std::{
    collections::VecDeque,
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use alef_core::registry::dialog::{
    ConfirmOptions, DialogCall, FileFilter, MessageKind, MessageOptions, OpenOptions, SaveOptions,
};
use rfd::{
    AsyncFileDialog, AsyncMessageDialog, FileHandle, MessageButtons, MessageDialogResult,
    MessageLevel,
};
use serde_json::Value;
use tokio::{runtime::Handle, sync::Semaphore};
use winit::window::Window;

use crate::ui::UiReply;

/// The answers an end-to-end run gives, in the order the documents ask: `[{"open": [...]}, ...]`
/// where each entry names the dialog and holds the JSON the dialog answers with.
type Script = Mutex<VecDeque<(String, Value)>>;

pub(in crate::window) struct Dialogs {
    /// One dialog at a time; the others wait their turn.
    turn: Arc<Semaphore>,
    script: Option<Arc<Script>>,
}

impl Dialogs {
    /// An end-to-end run (`ALEF_E2E=1`) always has a script, an empty one when none was given, so
    /// no such run can show a dialog.
    pub(in crate::window) fn new() -> Self {
        let e2e = std::env::var("ALEF_E2E").is_ok_and(|value| value == "1");
        let script = e2e.then(|| {
            let text = std::env::var("ALEF_E2E_DIALOGS").unwrap_or_else(|_| "[]".to_owned());
            let entries = parse_script(&text).unwrap_or_else(|error| {
                eprintln!("ALEF_E2E_DIALOGS is unusable, every dialog will fail: {error}");
                VecDeque::new()
            });
            Arc::new(Mutex::new(entries))
        });
        Self {
            turn: Arc::new(Semaphore::new(1)),
            script,
        }
    }

    /// Shows `call` on top of `parent` and answers `reply` when the user is done.
    pub(in crate::window) fn show(
        &self,
        runtime: &Handle,
        title: &str,
        parent: Option<&Window>,
        call: DialogCall,
        reply: UiReply,
    ) {
        let turn = self.turn.clone();
        let source = match &self.script {
            Some(script) => Source::Script(script.clone(), call),
            None => Source::Native(prepare(title, parent, call)),
        };
        runtime.spawn(async move {
            let answer = one_at_a_time(&turn, || reply.canceled(), source.answer()).await;
            if let Some(answer) = answer {
                reply.finish(answer);
            }
        });
    }
}

/// Waits for the turn, then runs `work`; nothing runs for a request that was given up meanwhile.
async fn one_at_a_time<T>(
    turn: &Semaphore,
    given_up: impl FnOnce() -> bool,
    work: impl Future<Output = io::Result<T>>,
) -> Option<io::Result<T>> {
    let Ok(_turn) = turn.acquire().await else {
        return Some(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "The dialogs are closed",
        )));
    };
    if given_up() {
        return None;
    }
    Some(work.await)
}

enum Source {
    Native(Prepared),
    Script(Arc<Script>, DialogCall),
}

impl Source {
    async fn answer(self) -> io::Result<Value> {
        match self {
            Self::Native(prepared) => prepared.run().await,
            Self::Script(script, call) => scripted(&script, &call),
        }
    }
}

fn kind_of(call: &DialogCall) -> &'static str {
    match call {
        DialogCall::Open(_) => "open",
        DialogCall::Save(_) => "save",
        DialogCall::Message(_) => "message",
        DialogCall::Confirm(_) => "confirm",
    }
}

fn parse_script(text: &str) -> Result<VecDeque<(String, Value)>, String> {
    let entries: Vec<serde_json::Map<String, Value>> =
        serde_json::from_str(text).map_err(|error| error.to_string())?;
    entries
        .into_iter()
        .map(|entry| {
            let mut pairs = entry.into_iter();
            match (pairs.next(), pairs.next()) {
                (Some(pair), None) => Ok(pair),
                _ => Err("an entry names one dialog: {\"open\": ...}".to_owned()),
            }
        })
        .collect()
}

/// The next answer of the script, if it is for this kind of dialog.
fn scripted(script: &Script, call: &DialogCall) -> io::Result<Value> {
    let kind = kind_of(call);
    let next = script.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
    match next {
        None => Err(io::Error::other(format!(
            "The script has no answer left for dialog.{kind}"
        ))),
        Some((expected, answer)) if expected == kind => Ok(answer),
        Some((expected, _)) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("The script expects dialog.{expected}, the document asked for dialog.{kind}"),
        )),
    }
}

/// A dialog built on the UI thread, the only place that may look at the window.
enum Prepared {
    Open {
        dialog: AsyncFileDialog,
        multiple: bool,
        directory: bool,
    },
    Save(AsyncFileDialog),
    Message(AsyncMessageDialog),
    Confirm {
        dialog: AsyncMessageDialog,
        confirming: Option<String>,
    },
}

fn prepare(title: &str, parent: Option<&Window>, call: DialogCall) -> Prepared {
    match call {
        DialogCall::Open(options) => {
            let (multiple, directory) = (
                options.multiple == Some(true),
                options.directory == Some(true),
            );
            Prepared::Open {
                dialog: open_dialog(options, parent),
                multiple,
                directory,
            }
        }
        DialogCall::Save(options) => Prepared::Save(save_dialog(options, parent)),
        DialogCall::Message(options) => Prepared::Message(message_dialog(title, options, parent)),
        DialogCall::Confirm(options) => {
            let confirming = options.custom_labels().map(|(ok, _)| ok.to_owned());
            Prepared::Confirm {
                dialog: confirm_dialog(title, options, parent),
                confirming,
            }
        }
    }
}

/// The folder a dialog starts in and the file name it offers: a folder as it is, anything else as
/// the file of its parent folder.
fn start_of(path: &str) -> (PathBuf, Option<String>) {
    let path = Path::new(path);
    if path.is_dir() {
        return (path.to_owned(), None);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    match path.parent() {
        Some(parent) => (parent.to_owned(), name),
        None => (path.to_owned(), None),
    }
}

/// The filters a dialog can show: `*` stands for every file, which a dialog without a filter shows.
fn usable_filters(filters: &[FileFilter]) -> Vec<&FileFilter> {
    filters
        .iter()
        .filter(|filter| !filter.extensions.iter().any(|e| e == "*"))
        .collect()
}

fn files_dialog(
    title: Option<String>,
    filters: Option<Vec<FileFilter>>,
    default_path: Option<String>,
    parent: Option<&Window>,
) -> AsyncFileDialog {
    let mut dialog = AsyncFileDialog::new();
    if let Some(title) = title {
        dialog = dialog.set_title(title);
    }
    for filter in usable_filters(&filters.unwrap_or_default()) {
        dialog = dialog.add_filter(&filter.name, &filter.extensions);
    }
    if let Some(path) = default_path {
        let (directory, name) = start_of(&path);
        dialog = dialog.set_directory(directory);
        if let Some(name) = name {
            dialog = dialog.set_file_name(name);
        }
    }
    match parent {
        Some(parent) => dialog.set_parent(parent),
        None => dialog,
    }
}

fn open_dialog(options: OpenOptions, parent: Option<&Window>) -> AsyncFileDialog {
    files_dialog(options.title, options.filters, options.default_path, parent)
}

fn save_dialog(options: SaveOptions, parent: Option<&Window>) -> AsyncFileDialog {
    files_dialog(options.title, options.filters, options.default_path, parent)
}

fn level_of(kind: Option<MessageKind>) -> MessageLevel {
    match kind.unwrap_or_default() {
        MessageKind::Info => MessageLevel::Info,
        MessageKind::Warning => MessageLevel::Warning,
        MessageKind::Error => MessageLevel::Error,
    }
}

fn message_dialog(
    title: &str,
    options: MessageOptions,
    parent: Option<&Window>,
) -> AsyncMessageDialog {
    let dialog = AsyncMessageDialog::new()
        .set_title(options.title.unwrap_or_else(|| title.to_owned()))
        .set_description(options.message)
        .set_level(level_of(options.kind))
        .set_buttons(MessageButtons::Ok);
    match parent {
        Some(parent) => dialog.set_parent(parent),
        None => dialog,
    }
}

fn confirm_dialog(
    title: &str,
    options: ConfirmOptions,
    parent: Option<&Window>,
) -> AsyncMessageDialog {
    let buttons = match options.custom_labels() {
        Some((ok, cancel)) => MessageButtons::OkCancelCustom(ok.to_owned(), cancel.to_owned()),
        None => MessageButtons::OkCancel,
    };
    let dialog = AsyncMessageDialog::new()
        .set_title(options.title.unwrap_or_else(|| title.to_owned()))
        .set_description(options.message)
        .set_level(MessageLevel::Info)
        .set_buttons(buttons);
    match parent {
        Some(parent) => dialog.set_parent(parent),
        None => dialog,
    }
}

/// Whether the button pressed confirms. Systems that cannot label buttons answer `Ok` or `Yes`.
fn confirmed(result: &MessageDialogResult, confirming: Option<&str>) -> bool {
    match result {
        MessageDialogResult::Ok | MessageDialogResult::Yes => true,
        MessageDialogResult::Custom(label) => Some(label.as_str()) == confirming,
        MessageDialogResult::No | MessageDialogResult::Cancel => false,
    }
}

fn path_text(handle: &FileHandle) -> io::Result<String> {
    handle
        .path()
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "The path is not valid Unicode"))
}

impl Prepared {
    async fn run(self) -> io::Result<Value> {
        match self {
            Self::Open {
                dialog,
                multiple,
                directory,
            } => {
                let chosen: Vec<FileHandle> = match (directory, multiple) {
                    (false, false) => dialog.pick_file().await.into_iter().collect(),
                    (false, true) => dialog.pick_files().await.unwrap_or_default(),
                    (true, false) => dialog.pick_folder().await.into_iter().collect(),
                    (true, true) => dialog.pick_folders().await.unwrap_or_default(),
                };
                let paths: io::Result<Vec<String>> = chosen.iter().map(path_text).collect();
                Ok(Value::from(paths?))
            }
            Self::Save(dialog) => match dialog.save_file().await {
                Some(handle) => Ok(Value::from(path_text(&handle)?)),
                None => Ok(Value::Null),
            },
            Self::Message(dialog) => {
                dialog.show().await;
                Ok(Value::Null)
            }
            Self::Confirm { dialog, confirming } => {
                let result = dialog.show().await;
                Ok(Value::Bool(confirmed(&result, confirming.as_deref())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    fn script_of(text: &str) -> Script {
        Mutex::new(parse_script(text).expect("a script"))
    }

    fn open_call() -> DialogCall {
        DialogCall::Open(OpenOptions::default())
    }

    fn confirm_call() -> DialogCall {
        DialogCall::Confirm(ConfirmOptions {
            title: None,
            message: "Sure?".to_owned(),
            ok_label: None,
            cancel_label: None,
        })
    }

    #[test]
    fn a_script_is_a_list_of_entries_that_each_name_one_dialog() {
        assert!(parse_script("[]").unwrap().is_empty());
        let entries = parse_script(r#"[{"open": ["a"]}, {"confirm": true}]"#).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("open".to_owned(), json!(["a"])));
        for bad in [
            "{}",
            "[1]",
            r#"[{}]"#,
            r#"[{"open": [], "save": null}]"#,
            "not json",
        ] {
            assert!(parse_script(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_script_answers_in_order_and_only_to_the_dialog_it_names() {
        let script = script_of(r#"[{"open": ["a"]}, {"confirm": true}, {"open": []}]"#);
        assert_eq!(scripted(&script, &open_call()).unwrap(), json!(["a"]));
        let wrong = scripted(&script, &open_call()).unwrap_err();
        assert_eq!(wrong.kind(), io::ErrorKind::InvalidData);
        assert!(wrong.to_string().contains("dialog.confirm"), "{wrong}");
        assert_eq!(scripted(&script, &open_call()).unwrap(), json!([]));
        let empty = scripted(&script, &confirm_call()).unwrap_err();
        assert!(empty.to_string().contains("no answer left"), "{empty}");
    }

    #[test]
    fn a_dialog_starts_in_a_folder_and_a_file_in_its_parent_folder() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().to_string_lossy().into_owned();
        assert_eq!(
            start_of(&folder),
            (directory.path().to_owned(), None),
            "a folder as it is"
        );
        let file = directory.path().join("report.txt");
        assert_eq!(
            start_of(&file.to_string_lossy()),
            (directory.path().to_owned(), Some("report.txt".to_owned())),
            "a file that is not there yet is offered by name"
        );
    }

    #[test]
    fn a_filter_for_every_file_is_left_out_because_no_filter_shows_every_file() {
        let filter = |name: &str, extensions: &[&str]| FileFilter {
            name: name.to_owned(),
            extensions: extensions.iter().map(|e| (*e).to_owned()).collect(),
        };
        let filters = [
            filter("Images", &["png", "jpg"]),
            filter("All", &["*"]),
            filter("Mixed", &["txt", "*"]),
            filter("Text", &["txt"]),
        ];
        let kept: Vec<&str> = usable_filters(&filters)
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(kept, ["Images", "Text"]);
    }

    #[test]
    fn the_button_decides_the_answer_of_a_confirmation() {
        let custom = |label: &str| MessageDialogResult::Custom(label.to_owned());
        assert!(confirmed(&MessageDialogResult::Ok, None));
        assert!(confirmed(&MessageDialogResult::Yes, None));
        assert!(!confirmed(&MessageDialogResult::Cancel, None));
        assert!(!confirmed(&MessageDialogResult::No, None));
        assert!(confirmed(&custom("Delete"), Some("Delete")));
        assert!(!confirmed(&custom("Keep"), Some("Delete")));
        assert!(!confirmed(&custom("Delete"), None));
        assert!(
            confirmed(&MessageDialogResult::Ok, Some("Delete")),
            "a system that cannot label its buttons answers Ok"
        );
    }

    #[test]
    fn a_message_is_an_info_unless_it_says_otherwise() {
        assert!(matches!(level_of(None), MessageLevel::Info));
        assert!(matches!(
            level_of(Some(MessageKind::Warning)),
            MessageLevel::Warning
        ));
        assert!(matches!(
            level_of(Some(MessageKind::Error)),
            MessageLevel::Error
        ));
    }

    #[test]
    fn every_kind_of_dialog_can_be_built_without_a_window() {
        // Nothing is shown: the builders only collect what the dialog will be given.
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().to_string_lossy().into_owned();
        let file = directory
            .path()
            .join("report.txt")
            .to_string_lossy()
            .into_owned();
        let filters = Some(vec![
            FileFilter {
                name: "Text".to_owned(),
                extensions: vec!["txt".to_owned(), "md".to_owned()],
            },
            FileFilter {
                name: "All".to_owned(),
                extensions: vec!["*".to_owned()],
            },
        ]);
        let calls = [
            DialogCall::Open(OpenOptions::default()),
            DialogCall::Open(OpenOptions {
                title: Some("Pick".to_owned()),
                filters: filters.clone(),
                multiple: Some(true),
                directory: None,
                default_path: Some(file.clone()),
            }),
            DialogCall::Open(OpenOptions {
                directory: Some(true),
                multiple: Some(true),
                default_path: Some(folder),
                ..OpenOptions::default()
            }),
            DialogCall::Save(SaveOptions {
                title: None,
                filters,
                default_path: Some(file),
            }),
            DialogCall::Message(MessageOptions {
                title: None,
                message: "Saved\nall of it".to_owned(),
                kind: Some(MessageKind::Error),
            }),
            confirm_call(),
            DialogCall::Confirm(ConfirmOptions {
                title: Some("Sure".to_owned()),
                message: "Delete?".to_owned(),
                ok_label: Some("Delete".to_owned()),
                cancel_label: None,
            }),
        ];
        for call in calls {
            let kind = kind_of(&call);
            match (kind, prepare("App", None, call)) {
                ("open", Prepared::Open { .. })
                | ("save", Prepared::Save(_))
                | ("message", Prepared::Message(_)) => {}
                ("confirm", Prepared::Confirm { .. }) => {}
                _ => panic!("dialog.{kind} was prepared as another kind"),
            }
        }
        let Prepared::Confirm { confirming, .. } = prepare(
            "App",
            None,
            DialogCall::Confirm(ConfirmOptions {
                title: None,
                message: "m".to_owned(),
                ok_label: Some("Delete".to_owned()),
                cancel_label: None,
            }),
        ) else {
            panic!("a confirmation");
        };
        assert_eq!(confirming.as_deref(), Some("Delete"));
    }

    #[tokio::test]
    async fn dialogs_take_turns() {
        let turn = Arc::new(Semaphore::new(1));
        let (active, overlapped) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let (turn, active, overlapped) = (turn.clone(), active.clone(), overlapped.clone());
            tasks.push(tokio::spawn(async move {
                one_at_a_time(&turn, || false, async {
                    if active.fetch_add(1, Ordering::SeqCst) != 0 {
                        overlapped.fetch_add(1, Ordering::SeqCst);
                    }
                    tokio::time::sleep(Duration::from_millis(15)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
            }));
        }
        for task in tasks {
            assert!(task.await.unwrap().is_some());
        }
        assert_eq!(overlapped.load(Ordering::SeqCst), 0, "two dialogs at once");
    }

    #[tokio::test]
    async fn a_request_given_up_while_waiting_shows_nothing() {
        let turn = Semaphore::new(1);
        let shown = AtomicUsize::new(0);
        let skipped = one_at_a_time(&turn, || true, async {
            shown.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert!(skipped.is_none());
        assert_eq!(shown.load(Ordering::SeqCst), 0);
        turn.close();
        let closed = one_at_a_time(&turn, || false, async { Ok(()) }).await;
        assert_eq!(
            closed.expect("an answer").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
