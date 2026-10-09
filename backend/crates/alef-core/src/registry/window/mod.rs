// SPDX-License-Identifier: MIT OR Apache-2.0
//! The window contract: what the `window` and `screen` modules ask of the process that owns the
//! windows (`Host::ui`), what it answers, and how a window definition becomes a placement.
use serde::{Deserialize, Serialize};

use super::dialog::DialogCall;
use crate::security::window::{Length, WindowDef};

pub mod geometry;
pub mod shortcut;

/// A point in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "core.ts")]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A rectangle in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "core.ts")]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width
            && point.y >= self.y
            && point.y < self.y + self.height
    }
}

/// A display. Logical pixels are physical ones divided by the scale factor of this display, so the
/// rectangles of displays with different scale factors do not tile exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "core.ts")]
pub struct MonitorInfo {
    /// Name reported by the system, when there is one.
    pub name: Option<String>,
    /// The whole display.
    pub bounds: Rect,
    /// The part of the display that is not taken by the task bar, Dock or panels.
    pub work_area: Rect,
    pub scale_factor: f64,
    pub primary: bool,
}

/// Edge or corner a drag-resize starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "core.ts")]
pub enum ResizeEdge {
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    NorthWest,
}

/// State of a window (`window.state`, event `runtime.window.state`). Sizes and positions are
/// logical pixels: the inner size and the position of the outer frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "core.ts")]
pub struct WindowInfo {
    pub label: String,
    /// Grows with every change, so a stale snapshot can be told from a newer one.
    pub revision: u32,
    pub title: String,
    pub width: f64,
    pub height: f64,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub scale_factor: f64,
    pub focused: bool,
    pub maximized: bool,
    /// `None` when the platform cannot tell.
    pub minimized: Option<bool>,
    /// `None` when the platform cannot tell.
    pub visible: Option<bool>,
    pub decorated: bool,
    pub resizable: bool,
    pub fullscreen: bool,
    pub always_on_top: bool,
    /// Page zoom of the document (1 = 100 %).
    pub zoom: f64,
    pub supports_drag_resize: bool,
}

/// What to do with a window. The serialized form is `{ "op": "setTitle", "title": ... }`; every
/// `window.<op>` command of the module takes the fields of its variant plus an optional `label`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum WindowOp {
    State,
    SetTitle {
        title: String,
    },
    SetSize {
        width: Length,
        height: Length,
    },
    SetPosition {
        x: Length,
        y: Length,
    },
    Center,
    Minimize,
    Maximize,
    Restore,
    ToggleMaximize,
    SetFullscreen {
        enabled: bool,
    },
    SetAlwaysOnTop {
        enabled: bool,
    },
    SetResizable {
        enabled: bool,
    },
    SetDecorations {
        enabled: bool,
    },
    /// A missing side keeps no limit on that axis.
    SetMinSize {
        width: Option<Length>,
        height: Option<Length>,
    },
    /// A missing side keeps no limit on that axis.
    SetMaxSize {
        width: Option<Length>,
        height: Option<Length>,
    },
    Show,
    Hide,
    Focus,
    /// Closes the window the way the user does: the document may refuse (`closeIntercept`).
    Close,
    /// Closes the window without asking the document.
    Destroy,
    StartDrag,
    StartResize {
        edge: ResizeEdge,
    },
    SetZoom {
        factor: f64,
    },
    /// The calling document wants to answer close requests of its own window (`closeAnswer`).
    CloseIntercept {
        enabled: bool,
    },
    CloseAnswer {
        id: u64,
        prevent: bool,
    },
}

impl WindowOp {
    /// Command names of the operations, `window.<name>`.
    pub const NAMES: [&'static str; 25] = [
        "state",
        "setTitle",
        "setSize",
        "setPosition",
        "center",
        "minimize",
        "maximize",
        "restore",
        "toggleMaximize",
        "setFullscreen",
        "setAlwaysOnTop",
        "setResizable",
        "setDecorations",
        "setMinSize",
        "setMaxSize",
        "show",
        "hide",
        "focus",
        "close",
        "destroy",
        "startDrag",
        "startResize",
        "setZoom",
        "closeIntercept",
        "closeAnswer",
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Self::State => "state",
            Self::SetTitle { .. } => "setTitle",
            Self::SetSize { .. } => "setSize",
            Self::SetPosition { .. } => "setPosition",
            Self::Center => "center",
            Self::Minimize => "minimize",
            Self::Maximize => "maximize",
            Self::Restore => "restore",
            Self::ToggleMaximize => "toggleMaximize",
            Self::SetFullscreen { .. } => "setFullscreen",
            Self::SetAlwaysOnTop { .. } => "setAlwaysOnTop",
            Self::SetResizable { .. } => "setResizable",
            Self::SetDecorations { .. } => "setDecorations",
            Self::SetMinSize { .. } => "setMinSize",
            Self::SetMaxSize { .. } => "setMaxSize",
            Self::Show => "show",
            Self::Hide => "hide",
            Self::Focus => "focus",
            Self::Close => "close",
            Self::Destroy => "destroy",
            Self::StartDrag => "startDrag",
            Self::StartResize { .. } => "startResize",
            Self::SetZoom { .. } => "setZoom",
            Self::CloseIntercept { .. } => "closeIntercept",
            Self::CloseAnswer { .. } => "closeAnswer",
        }
    }
}

/// An operation on one window: the one with `label`, or the window of the calling document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowCall {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(flatten)]
    pub op: WindowOp,
}

/// A request for the thread that owns the windows.
#[derive(Debug, Clone, PartialEq)]
pub enum UiCall {
    Window(WindowCall),
    /// `window.create`: the permission was checked by the registry.
    Create(Box<WindowDef>),
    /// `window.all`.
    Windows,
    /// `screen.monitors`.
    Monitors,
    /// `screen.cursorPosition`.
    CursorPosition,
    /// `dialog.*`: a native dialog on top of the window of the caller; the options are checked.
    Dialog(DialogCall),
    /// `shortcut.*`: owned by a document session, including teardown after reload.
    Shortcut(shortcut::ShortcutCall),
}

#[cfg(test)]
mod tests;
