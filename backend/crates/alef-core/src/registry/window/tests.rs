// SPDX-License-Identifier: MIT OR Apache-2.0
use serde_json::{json, Value};

use super::geometry::{self, Axis};
use super::*;
use crate::{
    security::window::{LengthUnit, Monitor},
    ErrorCode,
};

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// A 1920x1080 primary display with a 40 px task bar, and a 1280x720 one (scale 2) to its right.
fn monitors() -> Vec<MonitorInfo> {
    vec![
        MonitorInfo {
            name: Some("primary".to_owned()),
            bounds: rect(0.0, 0.0, 1920.0, 1080.0),
            work_area: rect(0.0, 0.0, 1920.0, 1040.0),
            scale_factor: 1.0,
            primary: true,
        },
        MonitorInfo {
            name: None,
            bounds: rect(1920.0, 0.0, 1280.0, 720.0),
            work_area: rect(1920.0, 40.0, 1280.0, 680.0),
            scale_factor: 2.0,
            primary: false,
        },
    ]
}

fn definition(extra: Value) -> WindowDef {
    let mut base = json!({"label": "w", "url": "/", "width": 800, "height": 600});
    base.as_object_mut()
        .expect("object")
        .extend(extra.as_object().expect("object").clone());
    serde_json::from_value(base).expect("window definition")
}

fn place(extra: Value) -> geometry::Placement {
    geometry::place(&definition(extra), &monitors(), None).expect("placement")
}

fn point(x: f64, y: f64) -> Point {
    Point { x, y }
}

#[test]
fn pixel_sizes_are_logical_and_centre_in_the_work_area() {
    let placed = place(json!({}));
    assert_eq!((placed.width, placed.height), (800.0, 600.0));
    assert_eq!(placed.position, Some(point(560.0, 220.0)));
    assert_eq!((placed.min_size, placed.max_size), (None, None));
}

#[test]
fn percent_of_work_and_of_screen_use_their_own_rectangle() {
    let work = place(json!({"width": "70%work", "height": "80%work"}));
    assert_eq!((work.width, work.height), (1344.0, 832.0));
    let screen = place(json!({"width": "50%screen", "height": "50%screen"}));
    assert_eq!((screen.width, screen.height), (960.0, 540.0));
    // 100%work is the work area, 100%screen the whole display: they differ by the task bar.
    let (work, screen) = (
        place(json!({"width": "100%work", "height": "100%work"})),
        place(json!({"width": "100%screen", "height": "100%screen"})),
    );
    assert_eq!(screen.height - work.height, 40.0);
    assert_eq!(work.position, Some(point(0.0, 0.0)));
}

#[test]
fn the_cursor_display_is_used_only_when_asked_and_known() {
    let on_second = Some(point(2000.0, 100.0));
    let ask = |monitor: &str, cursor: Option<Point>| {
        let window =
            definition(json!({"width": "50%work", "height": "50%work", "monitor": monitor}));
        geometry::place(&window, &monitors(), cursor).expect("placement")
    };
    let second = ask("cursor", on_second);
    assert_eq!((second.width, second.height), (640.0, 340.0));
    assert_eq!(second.position, Some(point(1920.0 + 320.0, 40.0 + 170.0)));
    for (monitor, cursor) in [
        ("primary", on_second),
        ("cursor", None),
        ("cursor", Some(point(-50.0, -50.0))),
    ] {
        let placed = ask(monitor, cursor);
        assert_eq!(
            (placed.width, placed.height),
            (960.0, 520.0),
            "{monitor} {cursor:?}"
        );
    }
}

#[test]
fn explicit_positions_are_desktop_pixels_or_offsets_into_the_referenced_rectangle() {
    let at = |x: Value, y: Value, monitor: &str| {
        place(json!({"position": {"x": x, "y": y}, "monitor": monitor}))
            .position
            .expect("position")
    };
    assert_eq!(at(json!(30), json!(40), "primary"), point(30.0, 40.0));
    assert_eq!(
        at(json!("10%work"), json!("50%work"), "primary"),
        point(192.0, 520.0)
    );
    assert_eq!(
        at(json!("10%screen"), json!("10%screen"), "primary"),
        point(192.0, 108.0)
    );
    let second = place(json!({
        "position": {"x": "50%work", "y": "10%work"},
        "monitor": "cursor"
    }));
    // No cursor was given, so the primary display is the reference.
    assert_eq!(second.position, Some(point(960.0, 104.0)));
    let with_cursor = geometry::place(
        &definition(json!({"position": {"x": "50%work", "y": "10%work"}, "monitor": "cursor"})),
        &monitors(),
        Some(point(2000.0, 100.0)),
    )
    .expect("placement");
    assert_eq!(
        with_cursor.position,
        Some(point(1920.0 + 640.0, 40.0 + 68.0))
    );
}

#[test]
fn the_size_is_clamped_to_the_limits_and_the_limits_are_reported() {
    let big =
        place(json!({"width": 5000, "height": 5000, "maxWidth": 1000, "maxHeight": "50%work"}));
    assert_eq!((big.width, big.height), (1000.0, 520.0));
    assert_eq!(big.max_size, Some((1000.0, 520.0)));
    let small = place(json!({"width": 100, "height": 100, "minWidth": 400, "minHeight": 300}));
    assert_eq!((small.width, small.height), (400.0, 300.0));
    assert_eq!(small.min_size, Some((400.0, 300.0)));
    let one_side = place(json!({"minWidth": 900}));
    assert_eq!(one_side.min_size, Some((900.0, 0.0)));
    assert_eq!(one_side.width, 900.0);
    let max_one_side = place(json!({"maxHeight": 500}));
    assert_eq!(max_one_side.max_size, Some((geometry::UNLIMITED, 500.0)));
    assert_eq!(max_one_side.height, 500.0);
}

#[test]
fn a_size_is_held_between_the_limits_that_are_set() {
    let min = Some((300.0, 200.0));
    let max = Some((500.0, 400.0));
    assert_eq!(
        geometry::clamp_size((900.0, 900.0), min, max),
        (500.0, 400.0)
    );
    assert_eq!(
        geometry::clamp_size((100.0, 100.0), min, max),
        (300.0, 200.0)
    );
    assert_eq!(
        geometry::clamp_size((400.0, 100.0), min, max),
        (400.0, 200.0)
    );
    assert_eq!(
        geometry::clamp_size((900.0, 900.0), min, None),
        (900.0, 900.0)
    );
    assert_eq!(
        geometry::clamp_size((100.0, 100.0), None, max),
        (100.0, 100.0)
    );
    assert_eq!(geometry::clamp_size((9.0, 9.0), None, None), (9.0, 9.0));
}

#[test]
fn contradictory_or_empty_sizes_are_refused() {
    for extra in [
        json!({"minWidth": 900, "maxWidth": "10%work"}),
        json!({"minHeight": "90%work", "maxHeight": 100}),
        json!({"width": 0}),
        json!({"height": 0.5}),
    ] {
        let error =
            geometry::place(&definition(extra.clone()), &monitors(), None).expect_err("refused");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{extra}");
    }
}

#[test]
fn without_a_display_pixels_work_and_percentages_are_not_available() {
    let none: [MonitorInfo; 0] = [];
    let centred = geometry::place(&definition(json!({})), &none, None).expect("pixels");
    assert_eq!(centred.position, None, "the system chooses");
    let at = geometry::place(
        &definition(json!({"position": {"x": 5, "y": 6}})),
        &none,
        None,
    )
    .expect("explicit position");
    assert_eq!(at.position, Some(point(5.0, 6.0)));
    for extra in [
        json!({"width": "50%work"}),
        json!({"position": {"x": "1%screen", "y": 0}}),
    ] {
        let error = geometry::place(&definition(extra), &none, None).expect_err("percent");
        assert_eq!(error.code, ErrorCode::NotAvailable);
    }
}

#[test]
fn a_window_larger_than_the_work_area_keeps_its_corner_on_it() {
    let placed = place(json!({"width": 4000, "height": 3000, "monitor": "primary"}));
    assert_eq!(placed.position, Some(point(0.0, 0.0)));
    let work = rect(100.0, 50.0, 400.0, 300.0);
    assert_eq!(geometry::center(&work, 200.0, 100.0), point(200.0, 150.0));
    assert_eq!(geometry::center(&work, 800.0, 100.0), point(100.0, 150.0));
}

#[test]
fn the_primary_display_is_the_flagged_one_else_the_first() {
    let mut displays = monitors();
    displays.reverse();
    let picked = geometry::pick(&displays, Monitor::Primary, None).expect("display");
    assert!(picked.primary);
    for display in &mut displays {
        display.primary = false;
    }
    let picked = geometry::pick(&displays, Monitor::Primary, None).expect("display");
    assert_eq!(picked.bounds.x, 1920.0, "the first one");
    assert!(geometry::pick(&[], Monitor::Cursor, Some(point(0.0, 0.0))).is_none());
}

#[test]
fn lengths_resolve_per_axis() {
    let display = monitors().remove(0);
    let work = |percent| Length::Percent(percent, LengthUnit::Work);
    let screen = |percent| Length::Percent(percent, LengthUnit::Screen);
    let resolve = |length, axis| geometry::extent(length, axis, Some(&display)).expect("extent");
    assert_eq!(resolve(work(50.0), Axis::Horizontal), 960.0);
    assert_eq!(resolve(work(50.0), Axis::Vertical), 520.0);
    assert_eq!(resolve(screen(50.0), Axis::Vertical), 540.0);
    assert_eq!(resolve(Length::Px(33.5), Axis::Vertical), 33.5);
    let side = geometry::limit(None, Some(Length::Px(10.0)), 7.0, Some(&display)).expect("limit");
    assert_eq!(side, Some((7.0, 10.0)));
    assert_eq!(
        geometry::limit(None, None, 7.0, Some(&display)).expect("limit"),
        None
    );
}

fn samples() -> Vec<WindowOp> {
    let px = |value| Length::Px(value);
    vec![
        WindowOp::State,
        WindowOp::SetTitle {
            title: "t".to_owned(),
        },
        WindowOp::SetSize {
            width: px(1.0),
            height: Length::Percent(5.0, LengthUnit::Work),
        },
        WindowOp::SetPosition {
            x: px(1.0),
            y: px(2.0),
        },
        WindowOp::Center,
        WindowOp::Minimize,
        WindowOp::Maximize,
        WindowOp::Restore,
        WindowOp::ToggleMaximize,
        WindowOp::SetFullscreen { enabled: true },
        WindowOp::SetAlwaysOnTop { enabled: true },
        WindowOp::SetResizable { enabled: false },
        WindowOp::SetDecorations { enabled: false },
        WindowOp::SetMinSize {
            width: Some(px(1.0)),
            height: None,
        },
        WindowOp::SetMaxSize {
            width: None,
            height: Some(px(2.0)),
        },
        WindowOp::Show,
        WindowOp::Hide,
        WindowOp::Focus,
        WindowOp::Close,
        WindowOp::Destroy,
        WindowOp::StartDrag,
        WindowOp::StartResize {
            edge: ResizeEdge::NorthWest,
        },
        WindowOp::SetZoom { factor: 1.5 },
        WindowOp::CloseIntercept { enabled: true },
        WindowOp::CloseAnswer {
            id: 9,
            prevent: true,
        },
    ]
}

#[test]
fn every_operation_has_its_command_name_and_survives_the_wire() {
    let samples = samples();
    assert_eq!(samples.len(), WindowOp::NAMES.len());
    let mut names: Vec<&str> = samples.iter().map(WindowOp::name).collect();
    assert_eq!(
        names,
        WindowOp::NAMES,
        "the table lists the operations in order"
    );
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), WindowOp::NAMES.len(), "names are unique");
    for op in samples {
        let call = WindowCall {
            label: Some("w".to_owned()),
            op: op.clone(),
        };
        let json = serde_json::to_value(&call).expect("serialize");
        assert_eq!(json["op"], op.name());
        assert_eq!(json["label"], "w");
        let back: WindowCall = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, call);
    }
}

#[test]
fn a_command_body_is_the_operation_fields_plus_an_optional_label() {
    let call: WindowCall =
        serde_json::from_value(json!({"op": "setTitle", "title": "x"})).expect("no label");
    assert_eq!(call.label, None);
    assert_eq!(
        call.op,
        WindowOp::SetTitle {
            title: "x".to_owned()
        }
    );
    let call: WindowCall = serde_json::from_value(json!({
        "label": "second", "op": "setSize", "width": 640, "height": "60%work"
    }))
    .expect("lengths as number and text");
    assert_eq!(call.label.as_deref(), Some("second"));
    assert_eq!(
        call.op,
        WindowOp::SetSize {
            width: Length::Px(640.0),
            height: Length::Percent(60.0, LengthUnit::Work),
        }
    );
    for bad in [
        json!({"op": "explode"}),
        json!({"op": "setTitle"}),
        json!({"op": "setTitle", "title": 3}),
        json!({"op": "setSize", "width": "wide", "height": 1}),
        json!({"op": "setSize", "width": -5, "height": 1}),
        json!({"op": "startResize", "edge": "up"}),
        json!({"label": 7, "op": "close"}),
        json!({"title": "no op"}),
    ] {
        assert!(
            serde_json::from_value::<WindowCall>(bad.clone()).is_err(),
            "{bad} must be refused"
        );
    }
}

#[test]
fn window_info_uses_camel_case_names() {
    let info = WindowInfo {
        label: "main".to_owned(),
        revision: 3,
        title: "T".to_owned(),
        width: 800.0,
        height: 600.0,
        x: Some(10.0),
        y: None,
        scale_factor: 1.25,
        focused: true,
        maximized: false,
        minimized: Some(false),
        visible: None,
        decorated: true,
        resizable: true,
        fullscreen: false,
        always_on_top: true,
        zoom: 1.0,
        supports_drag_resize: true,
    };
    let json = serde_json::to_value(&info).expect("serialize");
    for key in [
        "scaleFactor",
        "alwaysOnTop",
        "supportsDragResize",
        "revision",
        "label",
    ] {
        assert!(json.get(key).is_some(), "{key}");
    }
    assert_eq!(json["y"], Value::Null);
    assert_eq!(
        serde_json::from_value::<WindowInfo>(json).expect("back"),
        info
    );
}
