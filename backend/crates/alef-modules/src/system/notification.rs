// SPDX-License-Identifier: MIT OR Apache-2.0
//! `notification`: a message the desktop shows outside the window. The text is checked here, the icon
//! (a file the document may read) is resolved here; the desktop is a [`NotificationBackend`].
//!
//! The system backend works on Linux (through `notify-send`). Windows and macOS show notifications
//! only for an application with an identity (an AppUserModelID, a signed bundle), which an
//! application run from a folder does not have: there `show` fails with `NOT_AVAILABLE` and says
//! why, until `alef install` gives the application one (M7b).
use std::{
    fmt::Debug,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use alef_core::{
    registry::{command::Reply, dispatch::Registry},
    security::permissions::Permission,
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::Value;

const TITLE_LIMIT: usize = 128;
const BODY_LIMIT: usize = 1024;

/// A notification as the desktop is asked to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    /// A file that exists, as the permission check resolved it.
    pub icon: Option<PathBuf>,
}

/// Where notifications go. Calls may block briefly; the module runs them off the async threads.
pub trait NotificationBackend: Send + Sync + Debug {
    fn show(&self, notification: &Notification) -> Result<(), AlefError>;
}

/// The notifications of the desktop of the user.
#[derive(Debug)]
pub struct SystemNotifications {
    /// The name the notifications are shown under.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    application: String,
    /// The program that shows them where there is one (`notify-send`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    program: std::ffi::OsString,
}

impl SystemNotifications {
    pub fn new(application: impl Into<String>) -> Self {
        Self {
            application: application.into(),
            program: "notify-send".into(),
        }
    }

    /// Shows notifications through `program` instead of `notify-send`.
    pub fn through(mut self, program: impl Into<std::ffi::OsString>) -> Self {
        self.program = program.into();
        self
    }
}

#[cfg(target_os = "linux")]
impl NotificationBackend for SystemNotifications {
    fn show(&self, notification: &Notification) -> Result<(), AlefError> {
        use std::process::{Command, Stdio};
        let mut command = Command::new(&self.program);
        command.arg("--app-name").arg(&self.application);
        if let Some(icon) = &notification.icon {
            command.arg("--icon").arg(icon);
        }
        // `--` ends the options: a title that starts with a dash is still a title.
        command.arg("--").arg(&notification.title);
        if !notification.body.is_empty() {
            command.arg(&notification.body);
        }
        let output = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .output()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    AlefError::new(
                        ErrorCode::NotAvailable,
                        "notification: notify-send is not installed",
                    )
                } else {
                    AlefError::new(ErrorCode::Internal, format!("notification: {error}"))
                }
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(AlefError::new(
                ErrorCode::Internal,
                format!(
                    "notification: notify-send failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ))
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl NotificationBackend for SystemNotifications {
    fn show(&self, _notification: &Notification) -> Result<(), AlefError> {
        let reason = if cfg!(windows) {
            "Windows shows notifications for an application with an AppUserModelID"
        } else {
            "macOS shows notifications for a signed application bundle"
        };
        Err(AlefError::new(
            ErrorCode::NotAvailable,
            format!(
                "notification: {reason}; an application run from a folder has none (`alef install` gives it one)"
            ),
        ))
    }
}

/// A desktop that says yes and shows nothing: for runs that must not put anything on the screen of
/// the user. Keeps what it was asked and, given a file, appends it there as one JSON line each.
#[derive(Debug, Default)]
pub struct PretendNotifications {
    log: Option<PathBuf>,
    shown: Mutex<Vec<Notification>>,
}

impl PretendNotifications {
    pub fn logging_to(path: PathBuf) -> Self {
        Self {
            log: Some(path),
            shown: Mutex::default(),
        }
    }

    /// What it was asked to show, in order.
    pub fn shown(&self) -> Vec<Notification> {
        self.shown.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl NotificationBackend for PretendNotifications {
    fn show(&self, notification: &Notification) -> Result<(), AlefError> {
        self.shown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(notification.clone());
        if let Some(path) = &self.log {
            let line = serde_json::json!({
                "title": notification.title,
                "body": notification.body,
                "icon": notification.icon.as_ref().map(|icon| icon.to_string_lossy()),
            });
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| writeln!(file, "{line}"))
                .map_err(|error| {
                    AlefError::new(ErrorCode::Internal, format!("notification log: {error}"))
                })?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShowArgs {
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    icon: Option<String>,
}

fn refuse(message: impl Into<String>) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// The arguments are an object: serde would also read a list of values as the fields.
fn args_of(value: Value) -> Result<ShowArgs, AlefError> {
    if !value.is_object() {
        return Err(refuse("notification.show: the arguments are an object"));
    }
    serde_json::from_value(value).map_err(|error| refuse(format!("notification.show: {error}")))
}

fn checked(args: &ShowArgs) -> Result<(), AlefError> {
    if args.title.trim().is_empty() {
        return Err(refuse("notification.show: title is empty"));
    }
    if args.title.chars().count() > TITLE_LIMIT || args.title.chars().any(char::is_control) {
        return Err(refuse(format!(
            "notification.show: title is longer than {TITLE_LIMIT} characters or has a control character"
        )));
    }
    let body = args.body.as_deref().unwrap_or_default();
    if body.chars().count() > BODY_LIMIT
        || body
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err(refuse(format!(
            "notification.show: body is longer than {BODY_LIMIT} characters or has a control character"
        )));
    }
    Ok(())
}

fn existing_file(path: &Path) -> Result<(), AlefError> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err(refuse("notification.show: the icon is not a file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(AlefError::new(
            ErrorCode::NotFound,
            format!("notification.show: {} does not exist", path.display()),
        )),
        Err(error) => Err(AlefError::new(
            ErrorCode::Internal,
            format!("notification.show: {error}"),
        )),
    }
}

pub(crate) fn register(
    registry: &mut Registry,
    backend: Arc<dyn NotificationBackend>,
) -> Result<(), AlefError> {
    registry
        .command::<Value>("notification.show")?
        .handler(move |ctx, value| {
            let backend = backend.clone();
            async move {
                let args = args_of(value)?;
                checked(&args)?;
                let icon = match &args.icon {
                    Some(icon) => {
                        let path = ctx.permissions.authorize_path(
                            Permission::FsRead,
                            Some(icon),
                            &ctx.grants(),
                        )?;
                        existing_file(&path)?;
                        Some(path)
                    }
                    None => None,
                };
                let notification = Notification {
                    title: args.title,
                    body: args.body.unwrap_or_default(),
                    icon,
                };
                tokio::task::spawn_blocking(move || backend.show(&notification))
                    .await
                    .map_err(|error| {
                        AlefError::new(ErrorCode::Internal, format!("notification: {error}"))
                    })??;
                Ok(Reply::Json(Value::Null))
            }
        })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A stand-in for `notify-send` that writes the arguments it was given, one per line.
    fn shim(directory: &Path, exit: i32) -> (PathBuf, PathBuf) {
        let record = directory.join("arguments.txt");
        let program = directory.join("notify-send");
        fs::write(
            &program,
            format!(
                "#!/bin/sh\nfor argument in \"$@\"; do printf '%s\\n' \"$argument\" >> '{}'; done\nexit {exit}\n",
                record.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        (program, record)
    }

    fn notification(title: &str, body: &str, icon: Option<PathBuf>) -> Notification {
        Notification {
            title: title.to_owned(),
            body: body.to_owned(),
            icon,
        }
    }

    #[test]
    fn the_program_gets_the_name_the_icon_and_the_text_after_the_end_of_the_options() {
        let directory = tempfile::tempdir().unwrap();
        let (program, record) = shim(directory.path(), 0);
        let backend = SystemNotifications::new("Alef Test").through(program);
        let icon = directory.path().join("icon.png");
        fs::write(&icon, "x").unwrap();
        backend
            .show(&notification(
                "--title; rm -rf",
                "two\nlines",
                Some(icon.clone()),
            ))
            .unwrap();
        let arguments: Vec<String> = fs::read_to_string(&record)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            arguments,
            [
                "--app-name",
                "Alef Test",
                "--icon",
                icon.to_str().unwrap(),
                "--",
                "--title; rm -rf",
                "two",
                "lines"
            ],
            "the text is one argument each, and `--` keeps it from being read as options"
        );
    }

    #[test]
    fn an_empty_body_and_no_icon_leave_those_arguments_out() {
        let directory = tempfile::tempdir().unwrap();
        let (program, record) = shim(directory.path(), 0);
        SystemNotifications::new("App")
            .through(program)
            .show(&notification("Hello", "", None))
            .unwrap();
        let text = fs::read_to_string(&record).unwrap();
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            ["--app-name", "App", "--", "Hello"]
        );
    }

    #[test]
    fn a_program_that_is_missing_is_not_available_and_one_that_fails_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let missing =
            SystemNotifications::new("App").through(directory.path().join("nothing-here"));
        let error = missing.show(&notification("t", "", None)).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotAvailable);
        assert!(error.message.contains("notify-send"), "{}", error.message);
        let (program, _) = shim(directory.path(), 3);
        let error = SystemNotifications::new("App")
            .through(program)
            .show(&notification("t", "", None))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn without_an_application_identity_the_system_says_why_it_cannot_show_anything() {
        let error = SystemNotifications::new("App")
            .show(&Notification {
                title: "t".to_owned(),
                body: String::new(),
                icon: None,
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotAvailable);
        assert!(error.message.contains("alef install"), "{}", error.message);
    }
}
