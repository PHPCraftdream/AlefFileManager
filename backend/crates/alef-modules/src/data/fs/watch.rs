// SPDX-License-Identifier: MIT OR Apache-2.0
//! `fs.watch`: what happens to a file or a folder, as a stream of events. The system tells in its
//! own way and often several times; the events of a short moment are put together and each is told
//! once. The watcher lives with the stream: when the page closes the stream or the document goes
//! away, the watch ends.
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use alef_core::{
    registry::{command::Reply, context::CallContext},
    AlefError, ErrorCode,
};
use notify::{
    event::{ModifyKind, RenameMode},
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use tokio::sync::mpsc;

use super::{
    dto::{WatchEvent, WatchKind},
    fault::coded,
    space::Place,
};
use crate::json;

/// Events of this long a moment are one batch.
const DEBOUNCE: Duration = Duration::from_millis(60);
/// What may wait for the stream to take it before events are given up.
const QUEUE: usize = 1024;

/// What one system event says, in terms of the places the application knows.
fn told(place: &Place, event: &Event) -> Vec<WatchEvent> {
    let show = |path: &Path| {
        place
            .shown_path(path)
            .map(|p| p.to_string_lossy().into_owned())
    };
    let one = |kind: WatchKind, path: &PathBuf| {
        show(path).map(|path| WatchEvent {
            kind,
            path,
            to: None,
        })
    };
    match &event.kind {
        EventKind::Create(_) => event
            .paths
            .iter()
            .filter_map(|p| one(WatchKind::Create, p))
            .collect(),
        EventKind::Remove(_) => event
            .paths
            .iter()
            .filter_map(|p| one(WatchKind::Remove, p))
            .collect(),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => match event.paths.as_slice() {
            [from, to] => match (show(from), show(to)) {
                (Some(path), Some(to)) => vec![WatchEvent {
                    kind: WatchKind::Rename,
                    path,
                    to: Some(to),
                }],
                (Some(path), None) => vec![WatchEvent {
                    kind: WatchKind::Remove,
                    path,
                    to: None,
                }],
                (None, Some(path)) => vec![WatchEvent {
                    kind: WatchKind::Create,
                    path,
                    to: None,
                }],
                (None, None) => Vec::new(),
            },
            _ => Vec::new(),
        },
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => event
            .paths
            .iter()
            .filter_map(|p| one(WatchKind::Rename, p))
            .collect(),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => event
            .paths
            .iter()
            .filter_map(|p| one(WatchKind::Create, p))
            .collect(),
        EventKind::Modify(_) | EventKind::Any | EventKind::Other => event
            .paths
            .iter()
            .filter_map(|p| one(WatchKind::Modify, p))
            .collect(),
        EventKind::Access(_) => Vec::new(),
    }
}

/// Starts the watch of `place` and answers with the stream of its events.
pub(super) async fn start(
    ctx: &CallContext,
    place: Place,
    recursive: bool,
) -> Result<Reply, AlefError> {
    let (events, mut arrived) = mpsc::channel::<notify::Result<Event>>(QUEUE);
    let lost = Arc::new(AtomicBool::new(false));
    let marker = lost.clone();
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            if events.try_send(event).is_err() {
                marker.store(true, Ordering::SeqCst);
            }
        },
        Config::default(),
    )
    .map_err(|_| coded(ErrorCode::NotAvailable))?;
    let target = place.clone();
    let mode = if recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    let watched = super::blocking(move || {
        target.prepare().map_err(super::fault::fault)?;
        // What is not there is told as such, whatever the watcher of the platform says.
        std::fs::metadata(&target.real).map_err(super::fault::fault)?;
        watcher
            .watch(&target.real, mode)
            .map_err(|error| match error.kind {
                notify::ErrorKind::PathNotFound => coded(ErrorCode::NotFound),
                notify::ErrorKind::MaxFilesWatch => coded(ErrorCode::Busy),
                _ => coded(ErrorCode::NotAvailable),
            })?;
        Ok(watcher)
    })
    .await?;
    let (writer, id) = ctx.streams().open_outgoing();
    let shown = place.shown.to_string_lossy().into_owned();
    tokio::spawn(async move {
        // The watcher stays here: dropping it ends the watch.
        let _watcher = watched;
        loop {
            let Some(first) = arrived.recv().await else {
                return;
            };
            let mut batch = told_all(&place, [first]);
            let moment = tokio::time::sleep(DEBOUNCE);
            tokio::pin!(moment);
            loop {
                tokio::select! {
                    () = &mut moment => break,
                    more = arrived.recv() => match more {
                        Some(event) => batch.extend(told_all(&place, [event])),
                        None => break,
                    },
                }
            }
            for event in tidy(batch, lost.swap(false, Ordering::SeqCst), &shown) {
                let frame = serde_json::to_value(&event).unwrap_or_default();
                if writer.send_json(frame).await.is_err() {
                    return;
                }
            }
        }
    });
    json(&serde_json::json!({ "stream": id.0 }))
}

/// The events of a moment: each told once, and a call to look again when events were lost.
fn tidy(batch: Vec<WatchEvent>, lost: bool, shown: &str) -> Vec<WatchEvent> {
    let mut told: Vec<WatchEvent> = Vec::new();
    let overflow = lost.then(|| WatchEvent {
        kind: WatchKind::Overflow,
        path: shown.to_owned(),
        to: None,
    });
    for event in batch.into_iter().chain(overflow) {
        if !told.contains(&event) {
            told.push(event);
        }
    }
    told
}

fn told_all(
    place: &Place,
    events: impl IntoIterator<Item = notify::Result<Event>>,
) -> Vec<WatchEvent> {
    events
        .into_iter()
        .flat_map(|event| match event {
            Ok(event) => told(place, &event),
            Err(_) => Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, CreateKind, DataChange, RemoveKind};

    use super::*;

    fn event(kind: EventKind, paths: &[&Path]) -> Event {
        paths.iter().fold(Event::new(kind), |event, path| {
            event.add_path(path.to_path_buf())
        })
    }

    fn happened(kind: WatchKind, path: &Path, to: Option<&Path>) -> WatchEvent {
        WatchEvent {
            kind,
            path: path.to_string_lossy().into_owned(),
            to: to.map(|to| to.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn what_the_system_tells_is_told_in_the_names_of_the_application() {
        let base = std::env::temp_dir();
        let (shown, real) = (base.join("shown"), base.join("real"));
        let place = Place::for_test(shown.clone(), real.clone(), None);
        let (a, b) = (real.join("a.txt"), real.join("b.txt"));
        let (name_a, name_b) = (shown.join("a.txt"), shown.join("b.txt"));
        let rename = |mode| EventKind::Modify(ModifyKind::Name(mode));
        let cases = [
            (
                event(EventKind::Create(CreateKind::File), &[&a]),
                vec![happened(WatchKind::Create, &name_a, None)],
            ),
            (
                event(EventKind::Remove(RemoveKind::File), &[&a]),
                vec![happened(WatchKind::Remove, &name_a, None)],
            ),
            (
                event(
                    EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                    &[&a],
                ),
                vec![happened(WatchKind::Modify, &name_a, None)],
            ),
            (
                event(EventKind::Any, &[&a]),
                vec![happened(WatchKind::Modify, &name_a, None)],
            ),
            (
                event(rename(RenameMode::Both), &[&a, &b]),
                vec![happened(WatchKind::Rename, &name_a, Some(&name_b))],
            ),
            (
                event(rename(RenameMode::From), &[&a]),
                vec![happened(WatchKind::Rename, &name_a, None)],
            ),
            (
                event(rename(RenameMode::To), &[&b]),
                vec![happened(WatchKind::Create, &name_b, None)],
            ),
            (event(EventKind::Access(AccessKind::Any), &[&a]), Vec::new()),
        ];
        for (system, expected) in cases {
            assert_eq!(told(&place, &system), expected, "{:?}", system.kind);
        }
    }

    #[test]
    fn a_stand_in_tells_only_what_happens_inside_it() {
        let base = std::env::temp_dir();
        let (shown, real) = (base.join("shown"), base.join("real"));
        let place = Place::for_test(shown.clone(), real.clone(), Some(base.join("root")));
        let (inside, other) = (real.join("a.txt"), base.join("elsewhere").join("b.txt"));
        let both = EventKind::Modify(ModifyKind::Name(RenameMode::Both));
        let leaves = told(&place, &event(both, &[&inside, &other]));
        assert_eq!(
            leaves,
            vec![happened(WatchKind::Remove, &shown.join("a.txt"), None)]
        );
        let arrives = told(&place, &event(both, &[&other, &inside]));
        assert_eq!(
            arrives,
            vec![happened(WatchKind::Create, &shown.join("a.txt"), None)]
        );
        assert!(told(&place, &event(both, &[&other, &other])).is_empty());
        assert!(told(
            &place,
            &event(EventKind::Create(CreateKind::File), &[&other])
        )
        .is_empty());
    }

    #[test]
    fn a_real_place_tells_a_path_in_another_spelling_as_it_came() {
        let base = std::env::temp_dir();
        let place = Place::for_test(base.join("shown"), base.join("real"), None);
        let other = base.join("elsewhere").join("c.txt");
        let got = told(
            &place,
            &event(EventKind::Create(CreateKind::File), &[&other]),
        );
        assert_eq!(got, vec![happened(WatchKind::Create, &other, None)]);
    }

    #[test]
    fn the_events_of_a_moment_are_told_once_and_lost_events_call_to_look_again() {
        let base = std::env::temp_dir();
        let (a, b) = (base.join("a"), base.join("b"));
        let first = happened(WatchKind::Modify, &a, None);
        let second = happened(WatchKind::Create, &b, None);
        let batch = vec![first.clone(), second.clone(), first.clone()];
        assert_eq!(
            tidy(batch, false, "shown"),
            vec![first.clone(), second.clone()]
        );
        let overflow = WatchEvent {
            kind: WatchKind::Overflow,
            path: "shown".into(),
            to: None,
        };
        assert_eq!(
            tidy(vec![first.clone()], true, "shown"),
            vec![first, overflow.clone()]
        );
        assert_eq!(tidy(Vec::new(), true, "shown"), vec![overflow]);
        assert!(tidy(Vec::new(), false, "shown").is_empty());
    }
}
