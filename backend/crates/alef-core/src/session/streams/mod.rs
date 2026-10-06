// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bidirectional stream hub with per-stream outgoing credit and incoming backpressure.
use crate::{
    error::{AlefError, ErrorCode},
    ids::StreamId,
    protocol::{
        credit::{chunk, AcquireError, CreditGate},
        frame::Frame,
    },
};
use bytes::Bytes;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};

fn closed() -> AlefError {
    AlefError::new(ErrorCode::Closed, "stream closed")
}
fn not_found() -> AlefError {
    AlefError::new(ErrorCode::NotFound, "stream not found")
}
struct Outgoing {
    id: StreamId,
    gate: CreditGate,
    /// `None` once terminal; the lock makes "check terminal + enqueue" atomic against termination.
    tx: Mutex<Option<mpsc::UnboundedSender<Frame>>>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Frame>>>,
    chunk: usize,
}
impl Outgoing {
    fn is_terminal(&self) -> bool {
        self.tx.lock().expect("outgoing mutex poisoned").is_none()
    }
    fn push(&self, frame: Frame) -> Result<(), AlefError> {
        let tx = self.tx.lock().expect("outgoing mutex poisoned");
        tx.as_ref()
            .ok_or_else(closed)?
            .send(frame)
            .map_err(|_| closed())
    }
    /// Queues the terminal frame once; later pushes fail with Closed.
    fn terminate(&self, frame: Frame) {
        if let Some(tx) = self.tx.lock().expect("outgoing mutex poisoned").take() {
            self.gate.close();
            let _ = tx.send(frame);
        }
    }
}
struct Incoming {
    id: StreamId,
    /// `None` once terminal.
    tx: Mutex<Option<mpsc::Sender<Bytes>>>,
    rx: Mutex<Option<mpsc::Receiver<Bytes>>>,
    reason: Arc<Mutex<Option<AlefError>>>,
    /// Flips to true on termination so a write blocked on backpressure wakes up.
    cancel: watch::Sender<bool>,
}
impl Incoming {
    /// Terminates once; the reason is stored before the channel closes so the reader always sees it.
    fn terminate(&self, reason: Option<AlefError>) {
        let mut tx = self.tx.lock().expect("incoming mutex poisoned");
        if tx.is_some() {
            *self.reason.lock().expect("reason mutex poisoned") = reason;
            self.cancel.send_replace(true);
            tx.take();
        }
    }
}
struct Entry {
    out: Option<Arc<Outgoing>>,
    input: Option<Arc<Incoming>>,
    taken: bool,
}
struct State {
    next: u64,
    closed: bool,
    entries: Vec<Entry>,
}
/// Thread-safe per-session collection of streams.
pub struct StreamHub {
    window: usize,
    chunk: usize,
    state: Mutex<State>,
}
impl StreamHub {
    /// Creates a hub; both sizes must be nonzero.
    pub fn new(window: usize, chunk_size: usize) -> Self {
        assert!(window > 0 && chunk_size > 0, "sizes must be nonzero");
        Self {
            window,
            chunk: chunk_size,
            state: Mutex::new(State {
                next: 1,
                closed: false,
                entries: Vec::new(),
            }),
        }
    }
    fn id(s: &mut State) -> StreamId {
        let id = StreamId(s.next);
        s.next = s.next.checked_add(1).expect("stream id exhausted");
        id
    }
    /// Opens an outgoing runtime-to-page stream.
    pub fn open_outgoing(&self) -> (StreamWriter, StreamId) {
        let mut h = self.state.lock().expect("hub mutex poisoned");
        let id = Self::id(&mut h);
        let (tx, rx) = mpsc::unbounded_channel();
        let o = Arc::new(Outgoing {
            id,
            gate: CreditGate::with_window(self.window),
            tx: Mutex::new(Some(tx)),
            rx: Mutex::new(Some(rx)),
            chunk: self.chunk,
        });
        if h.closed {
            o.terminate(Frame::Error(closed()));
        }
        h.entries.push(Entry {
            out: Some(o.clone()),
            input: None,
            taken: false,
        });
        (StreamWriter { o, done: false }, id)
    }
    /// Opens a bounded page-to-runtime stream.
    pub fn open_incoming(&self) -> (IncomingWriter, StreamId) {
        let mut h = self.state.lock().expect("hub mutex poisoned");
        let id = Self::id(&mut h);
        let (tx, rx) = mpsc::channel((self.window / self.chunk).max(1));
        let i = Arc::new(Incoming {
            id,
            tx: Mutex::new(Some(tx)),
            rx: Mutex::new(Some(rx)),
            reason: Arc::new(Mutex::new(None)),
            cancel: watch::channel(false).0,
        });
        if h.closed {
            i.terminate(Some(closed()));
        }
        h.entries.push(Entry {
            out: None,
            input: Some(i.clone()),
            taken: false,
        });
        (IncomingWriter { i, done: false }, id)
    }
    /// Opens a page-to-runtime stream, returning its reader; the writer stays reachable by id via [`StreamHub::incoming_writer`].
    pub fn open_incoming_reader(&self) -> (IncomingReader, StreamId) {
        let mut h = self.state.lock().expect("hub mutex poisoned");
        let id = Self::id(&mut h);
        let (tx, rx) = mpsc::channel((self.window / self.chunk).max(1));
        let i = Arc::new(Incoming {
            id,
            tx: Mutex::new(Some(tx)),
            rx: Mutex::new(None),
            reason: Arc::new(Mutex::new(None)),
            cancel: watch::channel(false).0,
        });
        if h.closed {
            i.terminate(Some(closed()));
        }
        let reader = IncomingReader {
            id,
            rx,
            reason: i.reason.clone(),
            reported: false,
        };
        h.entries.push(Entry {
            out: None,
            input: Some(i),
            taken: true,
        });
        (reader, id)
    }
    /// Returns a disarmed writer for a hub-retained incoming stream; dropping it does not terminate the stream.
    pub fn incoming_writer(&self, id: StreamId) -> Option<IncomingWriter> {
        let h = self.state.lock().ok()?;
        let i = h
            .entries
            .iter()
            .find_map(|e| e.input.as_ref().filter(|i| i.id == id))?
            .clone();
        Some(IncomingWriter { i, done: true })
    }
    /// Takes the outgoing reader once.
    pub fn reader(&self, id: StreamId) -> Option<StreamReader> {
        let mut h = self.state.lock().ok()?;
        let e = h
            .entries
            .iter_mut()
            .find(|e| e.out.as_ref().is_some_and(|o| o.id == id))?;
        if e.taken {
            return None;
        }
        let o = e.out.as_ref()?.clone();
        let rx = o.rx.lock().ok()?.take()?;
        e.taken = true;
        Some(StreamReader {
            id,
            rx,
            delivered: false,
        })
    }
    /// Takes the incoming reader once.
    pub fn incoming_reader(&self, id: StreamId) -> Option<IncomingReader> {
        let mut h = self.state.lock().ok()?;
        let e = h
            .entries
            .iter_mut()
            .find(|e| e.input.as_ref().is_some_and(|i| i.id == id))?;
        if e.taken {
            return None;
        }
        let i = e.input.as_ref()?.clone();
        let rx = i.rx.lock().ok()?.take()?;
        let reason = i.reason.clone();
        e.taken = true;
        Some(IncomingReader {
            id,
            rx,
            reason,
            reported: false,
        })
    }
    /// Grants outgoing credit; unknown ids return NotFound.
    pub fn ack(&self, id: StreamId, n: usize) -> Result<(), AlefError> {
        let h = self.state.lock().expect("hub mutex poisoned");
        h.entries
            .iter()
            .find_map(|e| e.out.as_ref().filter(|o| o.id == id))
            .ok_or_else(not_found)?
            .gate
            .grant(n);
        Ok(())
    }
    /// Cancels an outgoing or incoming stream after buffered frames; unknown ids return NotFound.
    pub fn close(&self, id: StreamId) -> Result<(), AlefError> {
        let h = self.state.lock().expect("hub mutex poisoned");
        if let Some(o) = h
            .entries
            .iter()
            .find_map(|e| e.out.as_ref().filter(|o| o.id == id))
        {
            o.terminate(Frame::Error(closed()));
            return Ok(());
        }
        if let Some(i) = h
            .entries
            .iter()
            .find_map(|e| e.input.as_ref().filter(|i| i.id == id))
        {
            i.terminate(Some(closed()));
            return Ok(());
        }
        Err(not_found())
    }
    /// Terminates every stream (session teardown); afterwards new streams start already terminated.
    pub fn close_all(&self) {
        let mut h = self.state.lock().expect("hub mutex poisoned");
        h.closed = true;
        for e in &h.entries {
            if let Some(o) = &e.out {
                o.terminate(Frame::Error(closed()));
            }
            if let Some(i) = &e.input {
                i.terminate(Some(closed()));
            }
        }
    }
    /// Returns outgoing outstanding bytes.
    pub fn outstanding(&self, id: StreamId) -> Option<usize> {
        let h = self.state.lock().ok()?;
        h.entries.iter().find_map(|e| {
            e.out
                .as_ref()
                .filter(|o| o.id == id)
                .map(|o| o.gate.outstanding())
        })
    }
    /// Reports whether the hub is closed.
    pub fn is_closed(&self) -> bool {
        self.state.lock().expect("hub mutex poisoned").closed
    }
}
/// Runtime-to-page stream writer; sequential awaits preserve frame order, concurrent writers interleave at frame granularity.
pub struct StreamWriter {
    o: Arc<Outgoing>,
    done: bool,
}
impl StreamWriter {
    /// Returns stream id.
    pub fn id(&self) -> StreamId {
        self.o.id
    }
    /// Serializes JSON, acquires payload credit, then queues it.
    pub async fn send_json(&self, v: serde_json::Value) -> Result<(), AlefError> {
        let n = serde_json::to_vec(&v)
            .map_err(|e| AlefError::new(ErrorCode::InvalidArgument, e.to_string()))?
            .len();
        self.acquire(n).await?;
        self.o.push(Frame::Json(v))
    }
    /// Sends binary in credit-controlled chunks.
    pub async fn send_binary(&self, b: Bytes) -> Result<(), AlefError> {
        for p in chunk(b, self.o.chunk) {
            self.acquire(p.len()).await?;
            self.o.push(Frame::Binary(p))?;
        }
        Ok(())
    }
    async fn acquire(&self, n: usize) -> Result<(), AlefError> {
        if self.o.is_terminal() {
            return Err(closed());
        }
        self.o.gate.acquire(n).await.map_err(|e| match e {
            AcquireError::Closed => closed(),
            AcquireError::TooLarge { .. } => {
                AlefError::new(ErrorCode::InvalidArgument, "payload exceeds window")
            }
        })
    }
    /// Enqueues terminal End without credit.
    pub fn end(mut self) {
        self.done = true;
        self.o.terminate(Frame::End)
    }
    /// Enqueues terminal Error.
    pub fn error(mut self, e: AlefError) {
        self.done = true;
        self.o.terminate(Frame::Error(e))
    }
}
impl Drop for StreamWriter {
    fn drop(&mut self) {
        if !self.done {
            self.o.terminate(Frame::Error(closed()));
        }
    }
}
/// Reader for outgoing frames.
pub struct StreamReader {
    id: StreamId,
    rx: mpsc::UnboundedReceiver<Frame>,
    delivered: bool,
}
impl StreamReader {
    /// Returns stream id.
    pub fn id(&self) -> StreamId {
        self.id
    }
    /// Reads next frame, then returns None after terminal delivery.
    pub async fn next_frame(&mut self) -> Option<Frame> {
        if self.delivered {
            return None;
        }
        let frame = self.rx.recv().await;
        if matches!(frame, Some(Frame::End | Frame::Error(_)) | None) {
            self.delivered = true
        }
        frame
    }
}
/// Page-to-runtime stream writer.
pub struct IncomingWriter {
    i: Arc<Incoming>,
    done: bool,
}
impl IncomingWriter {
    /// Returns stream id.
    pub fn id(&self) -> StreamId {
        self.i.id
    }
    /// Writes a chunk with bounded backpressure; termination wakes a blocked write with Closed.
    pub async fn write(&self, b: Bytes) -> Result<(), AlefError> {
        let tx = self
            .i
            .tx
            .lock()
            .expect("incoming mutex poisoned")
            .as_ref()
            .cloned()
            .ok_or_else(closed)?;
        let mut cancelled = self.i.cancel.subscribe();
        tokio::select! {
            biased;
            _ = cancelled.wait_for(|c| *c) => Err(closed()),
            sent = tx.send(b) => sent.map_err(|_| closed()),
        }
    }
    /// Ends input cleanly.
    pub fn end(mut self) {
        self.done = true;
        self.i.terminate(None)
    }
    /// Aborts input after queued chunks with the supplied error.
    pub fn abort(mut self, e: AlefError) {
        self.done = true;
        self.i.terminate(Some(e))
    }
}
impl Drop for IncomingWriter {
    fn drop(&mut self) {
        if !self.done {
            self.i.terminate(Some(closed()));
        }
    }
}

/// Runtime-side reader of a page-to-runtime stream.
pub struct IncomingReader {
    id: StreamId,
    rx: mpsc::Receiver<Bytes>,
    reason: Arc<Mutex<Option<AlefError>>>,
    reported: bool,
}
impl IncomingReader {
    /// Returns stream id.
    pub fn id(&self) -> StreamId {
        self.id
    }
    /// Reads chunks, then reports terminal result once.
    pub async fn recv(&mut self) -> Option<Result<Bytes, AlefError>> {
        if let Some(b) = self.rx.recv().await {
            return Some(Ok(b));
        }
        if self.reported {
            return None;
        }
        self.reported = true;
        self.reason.lock().ok()?.take().map(Err)
    }
}

#[cfg(test)]
mod tests;
