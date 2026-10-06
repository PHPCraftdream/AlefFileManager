use super::*;
use std::time::Duration;
use tokio::time::timeout;
async fn bounded<T>(f: impl std::future::Future<Output = T>) -> T {
    timeout(Duration::from_secs(10), f)
        .await
        .expect("must not time out")
}
fn err<T: std::fmt::Debug>(r: Result<T, AlefError>, c: ErrorCode) {
    assert_eq!(r.expect_err("expected error").code, c);
}
async fn frame(r: &mut StreamReader) -> Option<Frame> {
    bounded(r.next_frame()).await
}

#[tokio::test]
async fn outgoing_orders_chunks_and_terminates() {
    let h = StreamHub::new(768, 256);
    let (w, id) = h.open_outgoing();
    let mut r = h.reader(id).expect("reader");
    let pattern = Bytes::from((0..700).map(|n| (n % 251) as u8).collect::<Vec<_>>());
    bounded(w.send_json(serde_json::json!({"x":1})))
        .await
        .expect("json");
    bounded(w.send_binary(pattern.clone()))
        .await
        .expect("binary");
    w.end();
    assert!(matches!(frame(&mut r).await, Some(Frame::Json(_))));
    let mut all = Vec::new();
    for len in [256, 256, 188] {
        match frame(&mut r).await {
            Some(Frame::Binary(b)) => {
                assert_eq!(b.len(), len);
                all.extend_from_slice(&b)
            }
            _ => panic!("binary frame"),
        }
    }
    assert_eq!(all, pattern);
    assert_eq!(frame(&mut r).await, Some(Frame::End));
    assert_eq!(frame(&mut r).await, None);
    assert_eq!(frame(&mut r).await, None);
}

#[tokio::test]
async fn credit_blocks_writer_until_ack() {
    let h = Arc::new(StreamHub::new(512, 256));
    let (w, id) = h.open_outgoing();
    let mut r = h.reader(id).expect("reader");
    let mut task = tokio::spawn(async move {
        for _ in 0..5 {
            w.send_binary(Bytes::from(vec![7; 256])).await?
        }
        w.end();
        Ok::<_, AlefError>(())
    });
    bounded(tokio::task::yield_now()).await;
    assert!(timeout(Duration::from_millis(100), &mut task)
        .await
        .is_err());
    assert_eq!(h.outstanding(id), Some(512));
    for _ in 0..5 {
        let f = frame(&mut r).await.expect("frame");
        h.ack(id, 256).expect("ack");
        assert!(h.outstanding(id).unwrap_or(usize::MAX) <= 512);
        assert!(matches!(f, Frame::Binary(_)));
    }
    bounded(task).await.expect("join").expect("writer");
    assert_eq!(frame(&mut r).await, Some(Frame::End));
    assert_eq!(frame(&mut r).await, None);
}

#[tokio::test]
async fn concurrent_writers_never_exceed_window() {
    let h = Arc::new(StreamHub::new(1024, 256));
    let (w, id) = h.open_outgoing();
    let w = Arc::new(w);
    let mut r = h.reader(id).expect("reader");
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let w = w.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..10 {
                w.send_binary(Bytes::from(vec![1; 100]))
                    .await
                    .expect("send")
            }
        }));
    }
    let mut max = 0;
    for n in 0..40 {
        assert!(matches!(frame(&mut r).await, Some(Frame::Binary(_))));
        max = max.max(h.outstanding(id).unwrap_or(usize::MAX));
        if n % 2 == 1 {
            h.ack(id, 200).expect("ack");
        }
    }
    assert!(max <= 1024);
    for t in tasks {
        bounded(t).await.expect("join");
    }
    drop(w);
    assert!(matches!(frame(&mut r).await,Some(Frame::Error(e)) if e.code==ErrorCode::Closed));
    assert_eq!(frame(&mut r).await, None);
}
#[tokio::test]
async fn close_midflight_fails_writes_and_terminates_reader() {
    let h = Arc::new(StreamHub::new(256, 256));
    let (w, id) = h.open_outgoing();
    let mut r = h.reader(id).expect("reader");
    let mut task = tokio::spawn(async move {
        for _ in 0..3 {
            w.send_binary(Bytes::from(vec![1; 256])).await?
        }
        Ok::<_, AlefError>(())
    });
    bounded(tokio::task::yield_now()).await;
    assert!(timeout(Duration::from_millis(100), &mut task)
        .await
        .is_err());
    h.close(id).expect("close");
    assert_eq!(
        bounded(task).await.expect("join").expect_err("closed").code,
        ErrorCode::Closed
    );
    assert!(matches!(frame(&mut r).await, Some(Frame::Binary(_))));
    assert!(matches!(frame(&mut r).await,Some(Frame::Error(e)) if e.code==ErrorCode::Closed));
    assert_eq!(frame(&mut r).await, None);
}
#[tokio::test]
async fn dropped_writer_terminates_stream() {
    let h = StreamHub::new(512, 256);
    let (w, id) = h.open_outgoing();
    let mut r = h.reader(id).expect("reader");
    bounded(w.send_json(serde_json::json!(1)))
        .await
        .expect("send");
    drop(w);
    assert!(matches!(frame(&mut r).await, Some(Frame::Json(_))));
    assert!(matches!(frame(&mut r).await,Some(Frame::Error(e)) if e.code==ErrorCode::Closed));
    assert_eq!(frame(&mut r).await, None);
    let (w, id) = h.open_outgoing();
    let mut r = h.reader(id).expect("reader");
    drop(w);
    assert!(matches!(frame(&mut r).await,Some(Frame::Error(e)) if e.code==ErrorCode::Closed));
    assert_eq!(frame(&mut r).await, None);
}
#[tokio::test]
async fn unknown_and_once_only_stream_ids() {
    let h = StreamHub::new(512, 256);
    err(h.ack(StreamId(99), 1), ErrorCode::NotFound);
    err(h.close(StreamId(99)), ErrorCode::NotFound);
    assert!(h.reader(StreamId(99)).is_none());
    let (w, id) = h.open_outgoing();
    let mut r = h.reader(id).expect("reader");
    assert!(h.reader(id).is_none());
    drop(w);
    assert!(matches!(frame(&mut r).await, Some(Frame::Error(_))));
    let (w, id) = h.open_incoming();
    let _r = h.incoming_reader(id).expect("reader");
    assert!(h.incoming_reader(id).is_none());
    drop(w);
    assert_ne!(h.open_outgoing().1, h.open_outgoing().1);
}
#[tokio::test]
async fn incoming_backpressures_and_ends() {
    let h = StreamHub::new(512, 256);
    let (w, id) = h.open_incoming();
    let mut r = h.incoming_reader(id).expect("reader");
    for _ in 0..2 {
        bounded(w.write(Bytes::from(vec![1; 256])))
            .await
            .expect("write");
    }
    let mut task = tokio::spawn(async move {
        let result = w.write(Bytes::from(vec![2; 256])).await;
        w.end();
        result
    });
    bounded(tokio::task::yield_now()).await;
    assert!(timeout(Duration::from_millis(100), &mut task)
        .await
        .is_err());
    assert!(matches!(bounded(r.recv()).await, Some(Ok(_))));
    bounded(task).await.expect("join").expect("write");
    assert!(matches!(bounded(r.recv()).await, Some(Ok(_))));
    assert!(matches!(bounded(r.recv()).await, Some(Ok(_))));
    assert_eq!(bounded(r.recv()).await, None);
}
#[tokio::test]
async fn incoming_abort_reports_reason() {
    let h = StreamHub::new(512, 256);
    let (w, id) = h.open_incoming();
    let mut r = h.incoming_reader(id).expect("reader");
    bounded(w.write(Bytes::from_static(b"x")))
        .await
        .expect("write");
    w.abort(AlefError::new(ErrorCode::InvalidArgument, "no"));
    assert!(matches!(bounded(r.recv()).await, Some(Ok(_))));
    assert!(matches!(bounded(r.recv()).await,Some(Err(e)) if e.code==ErrorCode::InvalidArgument));
    assert_eq!(bounded(r.recv()).await, None);
    assert_eq!(bounded(r.recv()).await, None);
}
#[tokio::test]
async fn incoming_writer_drop_terminates() {
    let h = StreamHub::new(512, 256);
    let (w, id) = h.open_incoming();
    let mut r = h.incoming_reader(id).expect("reader");
    bounded(w.write(Bytes::from_static(b"x")))
        .await
        .expect("write");
    drop(w);
    assert!(matches!(bounded(r.recv()).await, Some(Ok(_))));
    assert!(matches!(bounded(r.recv()).await,Some(Err(e)) if e.code==ErrorCode::Closed));
    assert_eq!(bounded(r.recv()).await, None);
}
#[tokio::test]
async fn close_all_terminates_every_stream() {
    let h = StreamHub::new(512, 256);
    let (w, id) = h.open_outgoing();
    let mut o = h.reader(id).expect("reader");
    bounded(w.send_json(serde_json::json!(1)))
        .await
        .expect("send");
    let (i, iid) = h.open_incoming();
    let mut r = h.incoming_reader(iid).expect("reader");
    bounded(i.write(Bytes::from_static(b"x")))
        .await
        .expect("write");
    h.close_all();
    assert!(matches!(frame(&mut o).await, Some(Frame::Json(_))));
    assert!(matches!(frame(&mut o).await,Some(Frame::Error(e)) if e.code==ErrorCode::Closed));
    assert_eq!(frame(&mut o).await, None);
    assert!(matches!(bounded(r.recv()).await, Some(Ok(_))));
    assert!(matches!(bounded(r.recv()).await,Some(Err(e)) if e.code==ErrorCode::Closed));
    assert_eq!(bounded(r.recv()).await, None);
    err(w.send_json(serde_json::json!(2)).await, ErrorCode::Closed);
    err(i.write(Bytes::new()).await, ErrorCode::Closed);
    assert!(h.is_closed());
    let (w, id) = h.open_outgoing();
    let mut r = h.reader(id).expect("reader");
    err(w.send_json(serde_json::json!(2)).await, ErrorCode::Closed);
    assert!(matches!(frame(&mut r).await,Some(Frame::Error(e)) if e.code==ErrorCode::Closed));
    assert_eq!(frame(&mut r).await, None);
}

#[tokio::test]
async fn close_all_wakes_write_blocked_on_backpressure() {
    // capacity 1: the second write blocks until the reader consumes or the stream terminates
    let h = Arc::new(StreamHub::new(256, 256));
    let (w, id) = h.open_incoming();
    let mut r = h.incoming_reader(id).expect("reader");
    bounded(w.write(Bytes::from_static(b"a")))
        .await
        .expect("fills the queue");
    let mut task = tokio::spawn(async move { w.write(Bytes::from_static(b"b")).await });
    bounded(tokio::task::yield_now()).await;
    assert!(timeout(Duration::from_millis(100), &mut task)
        .await
        .is_err());
    h.close_all();
    err(bounded(task).await.expect("join"), ErrorCode::Closed);
    assert_eq!(
        bounded(r.recv()).await.expect("queued chunk").expect("ok"),
        Bytes::from_static(b"a")
    );
    assert!(matches!(bounded(r.recv()).await, Some(Err(e)) if e.code == ErrorCode::Closed));
    assert_eq!(bounded(r.recv()).await, None);
}

/// Real parallelism: no send that returned Ok may be lost behind the terminal frame.
#[test]
fn outgoing_close_never_strands_accepted_sends() {
    for round in 0..200u64 {
        let h = Arc::new(StreamHub::new(1 << 20, 1 << 10));
        let (w, id) = h.open_outgoing();
        let mut r = h.reader(id).expect("reader");
        let writer = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            rt.block_on(async move {
                let mut accepted = 0usize;
                while w.send_json(serde_json::json!(accepted)).await.is_ok() {
                    accepted += 1;
                }
                accepted
            })
        });
        std::thread::sleep(Duration::from_micros(50 + round % 7 * 30));
        h.close(id).expect("close");
        let accepted = writer.join().expect("writer thread");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let mut seen = 0usize;
            loop {
                match frame(&mut r).await {
                    Some(Frame::Json(_)) => seen += 1,
                    Some(Frame::Error(e)) => {
                        assert_eq!(e.code, ErrorCode::Closed);
                        break;
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
            assert_eq!(seen, accepted, "round {round}");
            assert_eq!(frame(&mut r).await, None);
        });
    }
}

#[tokio::test]
async fn open_incoming_reader_keeps_writer_reachable_by_id() {
    let h = StreamHub::new(512, 256);
    let (mut reader, id) = h.open_incoming_reader();
    let writer = h.incoming_writer(id).expect("writer");
    bounded(writer.write(Bytes::from_static(b"chunk")))
        .await
        .expect("write");
    assert_eq!(
        bounded(reader.recv()).await,
        Some(Ok(Bytes::from_static(b"chunk")))
    );
    assert!(h.incoming_reader(id).is_none());
    assert!(h.incoming_writer(StreamId(999)).is_none());
    let (_, outgoing_id) = h.open_outgoing();
    assert!(h.incoming_writer(outgoing_id).is_none());
}

#[tokio::test]
async fn dropping_the_hub_writer_does_not_terminate() {
    let h = StreamHub::new(512, 256);
    let (mut reader, id) = h.open_incoming_reader();
    let writer = h.incoming_writer(id).expect("writer");
    bounded(writer.write(Bytes::from_static(b"chunk")))
        .await
        .expect("write");
    drop(writer);
    assert_eq!(
        bounded(reader.recv()).await,
        Some(Ok(Bytes::from_static(b"chunk")))
    );
    let second = h.incoming_writer(id).expect("second writer");
    second.end();
    assert_eq!(bounded(reader.recv()).await, None);
}

#[tokio::test]
async fn hub_close_cancels_incoming_reader_and_writer() {
    let h = StreamHub::new(512, 256);
    let (mut reader, id) = h.open_incoming_reader();
    let writer = h.incoming_writer(id).expect("writer");
    bounded(writer.write(Bytes::from_static(b"chunk")))
        .await
        .expect("write");
    h.close(id).expect("close");
    err(
        h.incoming_writer(id)
            .expect("writer")
            .write(Bytes::from_static(b"later"))
            .await,
        ErrorCode::Closed,
    );
    assert_eq!(
        bounded(reader.recv()).await,
        Some(Ok(Bytes::from_static(b"chunk")))
    );
    assert!(matches!(bounded(reader.recv()).await, Some(Err(e)) if e.code == ErrorCode::Closed));
    assert_eq!(bounded(reader.recv()).await, None);
    err(h.close(StreamId(999999)), ErrorCode::NotFound);
}
