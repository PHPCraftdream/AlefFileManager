// SPDX-License-Identifier: MIT OR Apache-2.0
//! The dialog contract: what the `dialog` module asks of the process that owns the windows
//! (`Host::ui`) and the checks the options pass first. The process shows the dialog on top of the
//! window of the calling document and answers with JSON:
//!
//! | call | answer |
//! |---|---|
//! | `Open` | array of absolute paths, empty when the user cancelled |
//! | `Save` | absolute path, `null` when the user cancelled |
//! | `Message` | `null` |
//! | `Confirm` | `true` for the confirming button |
use std::path::Path;

use serde::{Deserialize, Serialize};

const TITLE_LIMIT: usize = 256;
const MESSAGE_LIMIT: usize = 8192;
const LABEL_LIMIT: usize = 64;
const PATH_LIMIT: usize = 4096;
const FILTER_LIMIT: usize = 32;
const EXTENSION_LIMIT: usize = 16;

/// A named group of file extensions of a file dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "core.ts")]
pub struct FileFilter {
    /// Text the dialog shows for the group.
    pub name: String,
    /// Extensions without the dot (`png`); `*` stands for every file.
    pub extensions: Vec<String>,
}

/// Options of `dialog.open`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "core.ts")]
pub struct OpenOptions {
    /// Title of the dialog.
    #[ts(optional)]
    pub title: Option<String>,
    /// File groups; not for a folder dialog.
    #[ts(optional)]
    pub filters: Option<Vec<FileFilter>>,
    /// Several choices at once; default `false`.
    #[ts(optional)]
    pub multiple: Option<bool>,
    /// Choose folders instead of files; default `false`.
    #[ts(optional)]
    pub directory: Option<bool>,
    /// Folder the dialog starts in, or a file whose folder and name it starts with; absolute.
    #[ts(optional)]
    pub default_path: Option<String>,
}

/// Options of `dialog.save`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "core.ts")]
pub struct SaveOptions {
    /// Title of the dialog.
    #[ts(optional)]
    pub title: Option<String>,
    /// File groups.
    #[ts(optional)]
    pub filters: Option<Vec<FileFilter>>,
    /// Folder the dialog starts in, or a file whose folder and name it starts with; absolute.
    #[ts(optional)]
    pub default_path: Option<String>,
}

/// The weight a message dialog gives its text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "core.ts")]
pub enum MessageKind {
    #[default]
    Info,
    Warning,
    Error,
}

/// Options of `dialog.message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "core.ts")]
pub struct MessageOptions {
    /// Title of the dialog.
    #[ts(optional)]
    pub title: Option<String>,
    /// The text.
    pub message: String,
    /// Default `info`.
    #[ts(optional)]
    pub kind: Option<MessageKind>,
}

/// Options of `dialog.confirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "core.ts")]
pub struct ConfirmOptions {
    /// Title of the dialog.
    #[ts(optional)]
    pub title: Option<String>,
    /// The question.
    pub message: String,
    /// Text of the confirming button. Without both labels the buttons are the system's; with one,
    /// the other reads `OK` or `Cancel`.
    #[ts(optional)]
    pub ok_label: Option<String>,
    /// Text of the declining button.
    #[ts(optional)]
    pub cancel_label: Option<String>,
}

/// The confirming label when the document gave only the other one.
pub const DEFAULT_OK_LABEL: &str = "OK";
/// The declining label when the document gave only the other one.
pub const DEFAULT_CANCEL_LABEL: &str = "Cancel";

/// A request for a native dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogCall {
    Open(OpenOptions),
    Save(SaveOptions),
    Message(MessageOptions),
    Confirm(ConfirmOptions),
}

fn plain(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

fn title(value: &Option<String>) -> Result<(), String> {
    match value {
        Some(text) if text.chars().count() > TITLE_LIMIT => {
            Err(format!("title is longer than {TITLE_LIMIT} characters"))
        }
        Some(text) if !plain(text) => Err("title has a control character".to_owned()),
        _ => Ok(()),
    }
}

/// A message may break lines; nothing else of the control characters.
fn message(text: &str) -> Result<(), String> {
    if text.chars().count() > MESSAGE_LIMIT {
        return Err(format!("message is longer than {MESSAGE_LIMIT} characters"));
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("message has a control character".to_owned());
    }
    Ok(())
}

fn label(name: &str, value: &Option<String>) -> Result<(), String> {
    match value {
        Some(text) if text.is_empty() => Err(format!("{name} is empty")),
        Some(text) if text.chars().count() > LABEL_LIMIT => {
            Err(format!("{name} is longer than {LABEL_LIMIT} characters"))
        }
        Some(text) if !plain(text) => Err(format!("{name} has a control character")),
        _ => Ok(()),
    }
}

/// Two separators at the start make a UNC, WebDAV or device path on Windows (`\\host\share`,
/// `\\host@SSL\x`, `\\?\`, `\\.\`): the system would reach for the host, and sign in to it, just to
/// look at the folder.
fn on_network_or_device(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('/' | '\\'), Some('/' | '\\'))
    )
}

fn start(value: &Option<String>) -> Result<(), String> {
    match value {
        Some(text) if text.len() > PATH_LIMIT => {
            Err(format!("defaultPath is longer than {PATH_LIMIT} bytes"))
        }
        Some(text) if on_network_or_device(text) => {
            Err("defaultPath is a network or device path".to_owned())
        }
        Some(text) if !plain(text) || !Path::new(text).is_absolute() => {
            Err("defaultPath is not an absolute path".to_owned())
        }
        _ => Ok(()),
    }
}

fn filters(value: &Option<Vec<FileFilter>>) -> Result<(), String> {
    let list = value.as_deref().unwrap_or_default();
    if list.len() > FILTER_LIMIT {
        return Err(format!("more than {FILTER_LIMIT} filters"));
    }
    for filter in list {
        if filter.name.is_empty()
            || filter.name.chars().count() > LABEL_LIMIT
            || !plain(&filter.name)
        {
            return Err("a filter needs a plain name of up to 64 characters".to_owned());
        }
        if filter.extensions.is_empty() || filter.extensions.len() > FILTER_LIMIT {
            return Err(format!(
                "filter {:?} needs 1 to {FILTER_LIMIT} extensions",
                filter.name
            ));
        }
        for extension in &filter.extensions {
            let known = extension == "*"
                || (!extension.is_empty()
                    && extension.len() <= EXTENSION_LIMIT
                    && extension
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+')));
            if !known {
                return Err(format!(
                    "extension {extension:?} is not letters, digits, `_`, `-`, `+` or `*`"
                ));
            }
        }
    }
    Ok(())
}

impl OpenOptions {
    /// Limits of the text and shape of the options; the reason when one is broken.
    pub fn check(&self) -> Result<(), String> {
        title(&self.title)?;
        start(&self.default_path)?;
        filters(&self.filters)?;
        if self.directory == Some(true) && self.filters.as_ref().is_some_and(|f| !f.is_empty()) {
            return Err("filters apply to files, not to a folder dialog".to_owned());
        }
        Ok(())
    }
}

impl SaveOptions {
    /// Limits of the text and shape of the options; the reason when one is broken.
    pub fn check(&self) -> Result<(), String> {
        title(&self.title)?;
        start(&self.default_path)?;
        filters(&self.filters)
    }
}

impl MessageOptions {
    /// Limits of the text; the reason when one is broken.
    pub fn check(&self) -> Result<(), String> {
        title(&self.title)?;
        message(&self.message)
    }
}

impl ConfirmOptions {
    /// The two button texts, `None` when the system's buttons stay.
    pub fn custom_labels(&self) -> Option<(&str, &str)> {
        (self.ok_label.is_some() || self.cancel_label.is_some()).then(|| self.labels())
    }

    fn labels(&self) -> (&str, &str) {
        (
            self.ok_label.as_deref().unwrap_or(DEFAULT_OK_LABEL),
            self.cancel_label.as_deref().unwrap_or(DEFAULT_CANCEL_LABEL),
        )
    }

    /// Limits of the text; the two labels must differ, the answer is told by the button.
    pub fn check(&self) -> Result<(), String> {
        title(&self.title)?;
        message(&self.message)?;
        label("okLabel", &self.ok_label)?;
        label("cancelLabel", &self.cancel_label)?;
        let (ok, cancel) = self.labels();
        if ok == cancel {
            return Err("okLabel and cancelLabel are the same text".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute() -> String {
        if cfg!(windows) { r"C:\Users" } else { "/home" }.to_owned()
    }

    fn open(json: serde_json::Value) -> Result<OpenOptions, serde_json::Error> {
        serde_json::from_value(json)
    }

    #[test]
    fn unknown_fields_are_refused_and_names_are_camel_case() {
        assert!(open(serde_json::json!({ "bogus": 1 })).is_err());
        let options = open(serde_json::json!({ "defaultPath": absolute(), "multiple": true }))
            .expect("known fields");
        assert_eq!(options.multiple, Some(true));
        assert_eq!(options.default_path, Some(absolute()));
        assert!(options.check().is_ok());
        let confirm: ConfirmOptions = serde_json::from_value(
            serde_json::json!({ "message": "Sure?", "okLabel": "Yes", "cancelLabel": "No" }),
        )
        .expect("confirm");
        assert_eq!(confirm.ok_label.as_deref(), Some("Yes"));
    }

    #[test]
    fn message_kinds_are_lowercase_words() {
        let options: MessageOptions =
            serde_json::from_value(serde_json::json!({ "message": "m", "kind": "warning" }))
                .expect("kind");
        assert_eq!(options.kind, Some(MessageKind::Warning));
        assert!(serde_json::from_value::<MessageOptions>(
            serde_json::json!({ "message": "m", "kind": "Warning" })
        )
        .is_err());
        assert!(serde_json::from_value::<MessageOptions>(serde_json::json!({})).is_err());
    }

    #[test]
    fn a_start_path_must_be_absolute_and_plain() {
        for bad in ["relative/dir", "", "C\u{0}:\\x"] {
            let options = OpenOptions {
                default_path: Some(bad.to_owned()),
                ..OpenOptions::default()
            };
            assert!(options.check().is_err(), "{bad:?}");
        }
        let long = OpenOptions {
            default_path: Some(format!("{}{}", absolute(), "x".repeat(PATH_LIMIT))),
            ..OpenOptions::default()
        };
        assert!(long.check().is_err());
    }

    #[test]
    fn a_start_path_cannot_reach_a_network_host_or_a_device() {
        for bad in [
            r"\\host\share\x",
            r"\\host@SSL\DavWWWRoot\x",
            r"\\?\C:\Users",
            r"\\?\UNC\host\share",
            r"\\.\pipe\x",
            "//host/share/x",
            r"/\host\share",
            r"\/host/share",
        ] {
            let message = OpenOptions {
                default_path: Some(bad.to_owned()),
                ..OpenOptions::default()
            }
            .check()
            .unwrap_err();
            assert!(message.contains("network or device"), "{bad:?}: {message}");
            let save = SaveOptions {
                default_path: Some(bad.to_owned()),
                ..SaveOptions::default()
            };
            assert!(save.check().is_err(), "{bad:?}");
        }
        for fine in [absolute(), format!("{}/sub", absolute())] {
            let options = OpenOptions {
                default_path: Some(fine.clone()),
                ..OpenOptions::default()
            };
            assert!(options.check().is_ok(), "{fine}");
        }
    }

    #[test]
    fn titles_and_messages_are_limited_and_free_of_control_characters() {
        let mut options = MessageOptions {
            title: Some("t".repeat(TITLE_LIMIT + 1)),
            message: "m".to_owned(),
            kind: None,
        };
        assert!(options.check().unwrap_err().contains("title"));
        options.title = Some("two\nlines".to_owned());
        assert!(options.check().unwrap_err().contains("control"));
        options.title = None;
        options.message = "line one\r\n\tline two".to_owned();
        assert!(options.check().is_ok(), "a message may break lines");
        options.message = "bell\u{7}".to_owned();
        assert!(options.check().unwrap_err().contains("control"));
        options.message = "m".repeat(MESSAGE_LIMIT + 1);
        assert!(options.check().unwrap_err().contains("message"));
        options.message = "m".repeat(MESSAGE_LIMIT);
        assert!(options.check().is_ok());
    }

    #[test]
    fn filters_need_names_and_plain_extensions() {
        let filter = |name: &str, extensions: &[&str]| FileFilter {
            name: name.to_owned(),
            extensions: extensions.iter().map(|e| (*e).to_owned()).collect(),
        };
        let with = |filters: Vec<FileFilter>| SaveOptions {
            filters: Some(filters),
            ..SaveOptions::default()
        };
        assert!(with(vec![
            filter("Images", &["png", "jpg"]),
            filter("All", &["*"])
        ])
        .check()
        .is_ok());
        for bad in [
            filter("", &["png"]),
            filter("Images", &[]),
            filter("Images", &[".png"]),
            filter("Images", &["p ng"]),
            filter("Images", &[""]),
            filter("Images", &["a".repeat(EXTENSION_LIMIT + 1).as_str()]),
            filter("Images", &["*.png"]),
        ] {
            assert!(with(vec![bad.clone()]).check().is_err(), "{bad:?}");
        }
        let many = (0..=FILTER_LIMIT).map(|_| filter("a", &["b"])).collect();
        assert!(with(many).check().is_err());
    }

    #[test]
    fn a_folder_dialog_takes_no_filters() {
        let filtered = OpenOptions {
            directory: Some(true),
            filters: Some(vec![FileFilter {
                name: "Images".to_owned(),
                extensions: vec!["png".to_owned()],
            }]),
            ..OpenOptions::default()
        };
        assert!(filtered.check().unwrap_err().contains("folder"));
        let empty = OpenOptions {
            directory: Some(true),
            filters: Some(Vec::new()),
            ..OpenOptions::default()
        };
        assert!(empty.check().is_ok());
    }

    #[test]
    fn confirm_labels_are_plain_and_differ() {
        let confirm = |ok: Option<&str>, cancel: Option<&str>| ConfirmOptions {
            title: None,
            message: "Sure?".to_owned(),
            ok_label: ok.map(str::to_owned),
            cancel_label: cancel.map(str::to_owned),
        };
        assert!(confirm(Some("Delete"), Some("Keep")).check().is_ok());
        assert!(confirm(Some("Delete"), None).check().is_ok());
        assert!(confirm(None, None).check().is_ok());
        assert!(confirm(Some("Same"), Some("Same")).check().is_err());
        assert!(
            confirm(None, Some("OK")).check().is_err(),
            "the label that stays is `OK`, the answer would be told apart by nothing"
        );
        assert!(confirm(Some("Cancel"), None).check().is_err());
        assert_eq!(confirm(None, None).custom_labels(), None);
        assert_eq!(
            confirm(Some("Delete"), None).custom_labels(),
            Some(("Delete", "Cancel"))
        );
        assert_eq!(
            confirm(None, Some("Keep")).custom_labels(),
            Some(("OK", "Keep"))
        );
        assert!(confirm(Some(""), None).check().is_err());
        assert!(confirm(None, Some("a\nb")).check().is_err());
        let long = "x".repeat(LABEL_LIMIT + 1);
        assert!(confirm(Some(long.as_str()), None).check().is_err());
    }
}
