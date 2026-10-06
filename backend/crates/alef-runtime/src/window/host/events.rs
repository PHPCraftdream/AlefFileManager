// SPDX-License-Identifier: MIT OR Apache-2.0
//! Window events the documents can subscribe to, derived from how a window changed.
use alef_core::registry::window::WindowInfo;
use serde_json::{json, Value};

pub(super) const MOVED: &str = "window.moved";
pub(super) const RESIZED: &str = "window.resized";
pub(super) const FOCUS: &str = "window.focus";
pub(super) const BLUR: &str = "window.blur";
pub(super) const CLOSE_REQUESTED: &str = "window.close-requested";

/// The events that tell a document how `previous` became `next`, in a fixed order.
pub(in crate::window) fn changes(
    previous: &WindowInfo,
    next: &WindowInfo,
) -> Vec<(&'static str, Value)> {
    let label = &next.label;
    let mut events = Vec::new();
    if (previous.x, previous.y) != (next.x, next.y) {
        events.push((MOVED, json!({"label": label, "x": next.x, "y": next.y})));
    }
    if (previous.width, previous.height) != (next.width, next.height) {
        events.push((
            RESIZED,
            json!({
                "label": label,
                "width": next.width,
                "height": next.height,
                "scaleFactor": next.scale_factor,
            }),
        ));
    }
    if previous.focused != next.focused {
        let name = if next.focused { FOCUS } else { BLUR };
        events.push((name, json!({"label": label})));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> WindowInfo {
        WindowInfo {
            label: "main".to_owned(),
            revision: 1,
            title: "T".to_owned(),
            width: 800.0,
            height: 600.0,
            x: Some(10.0),
            y: Some(20.0),
            scale_factor: 1.0,
            focused: false,
            maximized: false,
            minimized: Some(false),
            visible: Some(true),
            decorated: true,
            resizable: true,
            fullscreen: false,
            always_on_top: false,
            zoom: 1.0,
            supports_drag_resize: true,
        }
    }

    fn names(events: &[(&'static str, Value)]) -> Vec<&'static str> {
        events.iter().map(|(name, _)| *name).collect()
    }

    #[test]
    fn nothing_that_matters_changed_means_no_event() {
        let before = window();
        let mut after = window();
        after.revision = 2;
        after.title = "Another".to_owned();
        after.maximized = true;
        after.always_on_top = true;
        assert!(changes(&before, &after).is_empty());
    }

    #[test]
    fn a_move_a_resize_and_a_focus_change_each_have_their_event() {
        let before = window();
        let mut moved = window();
        moved.x = Some(11.0);
        let events = changes(&before, &moved);
        assert_eq!(names(&events), [MOVED]);
        assert_eq!(events[0].1, json!({"label": "main", "x": 11.0, "y": 20.0}));

        let mut resized = window();
        resized.height = 650.0;
        resized.scale_factor = 1.5;
        let events = changes(&before, &resized);
        assert_eq!(names(&events), [RESIZED]);
        assert_eq!(
            events[0].1,
            json!({"label": "main", "width": 800.0, "height": 650.0, "scaleFactor": 1.5})
        );

        let mut focused = window();
        focused.focused = true;
        assert_eq!(names(&changes(&before, &focused)), [FOCUS]);
        assert_eq!(names(&changes(&focused, &before)), [BLUR]);
    }

    #[test]
    fn several_changes_come_in_a_fixed_order_and_an_unknown_position_is_null() {
        let before = window();
        let mut after = window();
        after.focused = true;
        after.width = 1.0;
        after.x = None;
        let events = changes(&before, &after);
        assert_eq!(names(&events), [MOVED, RESIZED, FOCUS]);
        assert_eq!(events[0].1["x"], Value::Null);
    }
}
