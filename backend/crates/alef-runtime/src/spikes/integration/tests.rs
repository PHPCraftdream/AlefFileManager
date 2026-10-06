// SPDX-License-Identifier: MIT OR Apache-2.0
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::dialog::{self, Dialog};
use super::forward::{spawn_forwarder, CrateSource, SpikeEvent, PROBE_EVENTS};
use super::verdict::Verdict;
use super::{drain_events, Cycle, Spike};
use std::sync::mpsc::channel;

// std mpsc Receiver has no by-value Iterator; wrap it with the same
// blocking-next semantics as crossbeam's `Receiver::iter()`.
#[allow(dead_code)]
struct OwnedIter<T>(std::sync::mpsc::Receiver<T>);

impl<T> Iterator for OwnedIter<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.0.recv().ok()
    }
}

#[allow(dead_code)]
fn test_spike(events: super::Receiver<SpikeEvent>) -> Spike {
    Spike {
        started: Instant::now(),
        events,
        timer_next: Arc::new(Mutex::new(None)),
        menu: None,
        tray: None,
        hotkey_ctrl: None,
        dialog: Dialog::new(false, Instant::now()),
        cycle: None,
        cycle_b: None,
        verdict: Verdict::default(),
        stops: Vec::new(),
        handles: Vec::new(),
        summary_done: false,
    }
}

// Forwarding layer under concurrent producers: lossless, per-producer order, no duplicates.
#[test]
fn forwarding_layer_is_lossless_ordered_and_non_duplicating() {
    const THREADS: usize = 4;
    const PER_THREAD: usize = 250;
    let (source_tx, source_rx) = std::sync::mpsc::channel::<(usize, usize)>();
    let (out_tx, out_rx) = channel::<SpikeEvent>();
    let handle = spawn_forwarder(
        "test-forwarder",
        OwnedIter(source_rx),
        |(tag, seq)| SpikeEvent::Probe(tag as u32 * 100_000 + seq as u32),
        out_tx,
        move || false,
    );
    let producers: Vec<_> = (0..THREADS)
        .map(|tag| {
            let sender = source_tx.clone();
            std::thread::spawn(move || {
                for seq in 0..PER_THREAD {
                    sender.send((tag, seq)).expect("source open");
                }
            })
        })
        .collect();
    drop(source_tx);
    for producer in producers {
        producer.join().expect("producer");
    }
    handle.join().expect("forwarder");
    let mut last_seq = [0usize; THREADS];
    let mut total = 0usize;
    for event in out_rx {
        let SpikeEvent::Probe(value) = event else {
            panic!("unexpected event variant");
        };
        let tag = (value / 100_000) as usize;
        let seq = (value % 100_000) as usize;
        assert!(tag < THREADS, "unknown tag");
        assert_eq!(seq, last_seq[tag], "order violated for tag {tag}");
        last_seq[tag] += 1;
        total += 1;
    }
    assert_eq!(total, THREADS * PER_THREAD, "lost or duplicated events");
}

// drain_events must accept exactly the in-order probe sequence through the real path.
#[test]
fn drain_events_accepts_exact_probe_sequence() {
    let (tx, rx) = channel::<SpikeEvent>();
    let mut spike = test_spike(rx);
    for index in 0..PROBE_EVENTS {
        tx.send(SpikeEvent::Probe(index)).unwrap();
    }
    drain_events(&mut spike);
    assert!(spike.verdict.forwarding_ok(PROBE_EVENTS));
}

// A lost event must break the verdict.
#[test]
fn drain_events_detects_loss() {
    let (tx, rx) = channel::<SpikeEvent>();
    let mut spike = test_spike(rx);
    tx.send(SpikeEvent::Probe(0)).unwrap();
    tx.send(SpikeEvent::Probe(2)).unwrap();
    drain_events(&mut spike);
    assert!(!spike.verdict.forwarding_ok(PROBE_EVENTS));
}

// A duplicated event must break the verdict.
#[test]
fn drain_events_detects_duplicate() {
    let (tx, rx) = channel::<SpikeEvent>();
    let mut spike = test_spike(rx);
    tx.send(SpikeEvent::Probe(0)).unwrap();
    tx.send(SpikeEvent::Probe(0)).unwrap();
    drain_events(&mut spike);
    assert!(!spike.verdict.forwarding_ok(PROBE_EVENTS));
}

// Forwarder threads must be joinable: `loop_closed` ends the thread promptly.
#[test]
fn forwarder_stops_when_loop_closed_fires() {
    let (tx, rx) = std::sync::mpsc::channel::<u32>();
    for value in 0..3u32 {
        tx.send(value).unwrap();
    }
    let (out_tx, out_rx) = channel::<SpikeEvent>();
    let handle = spawn_forwarder(
        "test-stop",
        OwnedIter(rx),
        SpikeEvent::Probe,
        out_tx,
        || true,
    );
    handle.join().expect("forwarder terminates on loop_closed");
    assert!(matches!(out_rx.try_recv(), Ok(SpikeEvent::Probe(0))));
    assert!(out_rx.try_recv().is_err(), "no events after stop");
}

// CrateSource must honor its stop flag instead of blocking on an idle channel.
#[test]
fn crate_source_stops_on_flag() {
    let (tx, rx) = crossbeam_channel::unbounded::<u32>();
    let stop = Arc::new(AtomicBool::new(false));
    let mut source = CrateSource::new(rx, stop.clone());
    tx.send(7).unwrap();
    assert_eq!(source.next(), Some(7));
    stop.store(true, Ordering::Relaxed);
    assert_eq!(source.next(), None);
}

// The dialog deadline chain drives detection, WM_CLOSE and gone-detection in order.
#[test]
fn dialog_deadline_follows_lifecycle() {
    let started = Instant::now();
    let mut dialog = Dialog::new(true, started);
    let mut cycle_b: Option<Cycle> = None;
    let now = started + std::time::Duration::from_secs(2);
    dialog::deadline(&dialog);
    dialog.found_at = Some(now);
    assert!(dialog::deadline(&dialog).is_some());
    dialog.close_posted_at = Some(now);
    assert!(dialog::deadline(&dialog).is_some());
    dialog.gone_at = Some(now);
    assert!(dialog::deadline(&dialog).is_none());
    let _ = &mut cycle_b;
    let _ = Cycle::new(now);
}

// One arm of the production drain path, shared by the drain tests.
#[allow(dead_code)]
fn drain_one(spike: &mut Spike, event: SpikeEvent) {
    match event {
        SpikeEvent::Probe(index) => spike.verdict.observe_probe(index),
        SpikeEvent::Tray(_) => spike.verdict.tray_events += 1,
        SpikeEvent::Menu(_) => spike.verdict.menu_events += 1,
        SpikeEvent::Hotkey(_) => spike.verdict.hotkey_events += 1,
    }
}
