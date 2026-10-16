// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unary calls: bodies, limits, permissions, panics, concurrency and client aborts.
use super::*;

async fn session_token(fx: &Fixture) -> String {
    open(fx, WINDOW).await.token().to_owned()
}

/// A JSON body large enough to be parsed on the parse executor.
fn big_args() -> Value {
    let pad = "x".repeat(crate::protocol::transport::call::INLINE_JSON_BODY + 1);
    json!({ "pad": pad })
}

fn error_code(response: &TransportResponse) -> String {
    body_json(response)["code"]
        .as_str()
        .expect("code")
        .to_owned()
}

#[tokio::test]
async fn json_calls_round_trip_and_an_empty_body_means_no_arguments() {
    let fx = fixture(config(Limits::default()));
    let token = session_token(&fx).await;
    let args = json!({"a": [1, 2, {"b": "привет"}], "n": null});
    let response = send(&fx, call("test.echo", &token, &args)).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(body_json(&response), args);
    let empty = request(Method::Post, "call/test.echo").token(&token);
    assert_eq!(body_json(&send(&fx, empty).await), json!({}));
}

#[tokio::test]
async fn binary_bodies_carry_their_arguments_in_the_percent_encoded_header() {
    let fx = fixture(config(Limits::default()));
    let token = session_token(&fx).await;
    let args = json!({"имя": "значение", "k": "a b&c"});
    let echo = request(Method::Post, "call/test.echo")
        .token(&token)
        .octets(Bytes::from_static(b"ignored"), &args);
    assert_eq!(
        body_json(&send(&fx, echo).await),
        args,
        "header decoded exactly"
    );
    let payload = Bytes::from_static(b"\x00\x01binary\xff");
    let bin = request(Method::Post, "call/test.bin")
        .token(&token)
        .octets(payload.clone(), &json!({}));
    let response = send(&fx, bin).await;
    assert_eq!(
        response.header("content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(body(&response), &payload);
}

#[tokio::test]
async fn sixteen_mebibytes_survive_the_round_trip_intact() {
    let fx = fixture(config(Limits::default()));
    let token = session_token(&fx).await;
    let size = 16 * 1024 * 1024;
    let data: Vec<u8> = (0..size)
        .map(|i: usize| (i.wrapping_mul(31) ^ (i >> 8)) as u8)
        .collect();
    let sum = |bytes: &[u8]| {
        bytes.iter().fold(0u64, |h, b| {
            h.wrapping_mul(1_099_511_628_211) ^ u64::from(*b)
        })
    };
    let expected = sum(&data);
    let bin = request(Method::Post, "call/test.bin")
        .token(&token)
        .octets(Bytes::from(data), &json!({}));
    let response = send(&fx, bin).await;
    assert_eq!(response.status, 200);
    assert_eq!(body(&response).len(), size);
    assert_eq!(sum(body(&response)), expected);
}

#[tokio::test]
async fn bodies_content_types_and_headers_are_validated() {
    let limits = Limits {
        max_unary_body: 64,
        max_bulk_body: 1024,
        ..Limits::default()
    };
    let fx = fixture(config(limits));
    let token = session_token(&fx).await;
    let post = || request(Method::Post, "call/test.echo").token(&token);
    let mut cases: Vec<(&str, TransportRequest, bool)> = Vec::new();
    let json_of = |len: usize| json!({"p": "x".repeat(len)});
    cases.push((
        "json at the limit",
        post().json(&json!({"p": "x".repeat(50)})),
        true,
    ));
    cases.push(("json over the limit", post().json(&json_of(80)), false));
    cases.push((
        "binary at the limit",
        post().octets(Bytes::from(vec![0; 1024]), &json!({})),
        true,
    ));
    cases.push((
        "binary over the limit",
        post().octets(Bytes::from(vec![0; 1025]), &json!({})),
        false,
    ));
    cases.push((
        "text/plain",
        post().header_set("content-type", "text/plain"),
        false,
    ));
    let mut not_json = post().header_set("content-type", "application/json");
    not_json.body = Bytes::from_static(b"{nope");
    cases.push(("invalid json", not_json, false));
    let bad_header = post()
        .header_set("content-type", "application/octet-stream")
        .header_set("x-alef-args", "%zz");
    cases.push(("invalid args header", bad_header, false));
    for (what, case, ok) in cases {
        let response = send(&fx, case).await;
        if ok {
            assert_eq!(response.status, 200, "{what}");
        } else {
            assert_eq!(response.status, 400, "{what}");
            assert_eq!(error_code(&response), "INVALID_ARGUMENT", "{what}");
        }
    }
}

#[tokio::test]
async fn a_permission_denial_stops_the_call_before_the_handler() {
    let fx = fixture(config(Limits::default()));
    let token = session_token(&fx).await;
    let outside = std::env::current_dir()
        .expect("cwd")
        .join("nowhere-granted");
    let args = json!({"path": outside.to_string_lossy()});
    let response = send(&fx, call("test.denied", &token, &args)).await;
    assert_eq!(response.status, 403);
    let error = body_json(&response);
    assert_eq!(error["code"], "PERMISSION_DENIED");
    assert_eq!(error["details"]["permission"], "fs.read");
    assert_eq!(
        fx.probe.handled.load(Ordering::SeqCst),
        0,
        "handler must not run"
    );
}

#[tokio::test]
async fn a_panicking_handler_is_an_internal_error_and_the_transport_keeps_working() {
    let fx = fixture(config(Limits::default()));
    let token = session_token(&fx).await;
    let response = send(&fx, call("test.panic", &token, &json!({}))).await;
    assert_eq!(response.status, 500);
    assert_eq!(error_code(&response), "INTERNAL");
    assert!(!format!("{response:?}").contains("panic-text"));
    assert!(!String::from_utf8_lossy(body(&response)).contains("panic-text"));
    let next = send(&fx, call("test.echo", &token, &json!({"after": true}))).await;
    assert_eq!(body_json(&next), json!({"after": true}));
}

#[tokio::test]
async fn call_slots_are_limited_per_session_and_released() {
    let limits = Limits {
        max_concurrent_calls_per_session: 2,
        ..Limits::default()
    };
    let fx = fixture(config(limits));
    let token = session_token(&fx).await;
    let spawn_slow = || {
        let transport = fx.transport.clone();
        let request = call("test.slow", &token, &json!({}));
        tokio::spawn(async move { transport.handle(request).await })
    };
    let running = [spawn_slow(), spawn_slow()];
    bounded(fx.probe.entered.acquire_many(2))
        .await
        .expect("entered")
        .forget();
    let third = send(&fx, call("test.echo", &token, &json!({}))).await;
    assert_eq!(third.status, 429);
    assert_eq!(error_code(&third), "BUSY");
    fx.probe.release.add_permits(2);
    for task in running {
        assert_eq!(bounded(task).await.expect("join").status, 200);
    }
    let after = send(&fx, call("test.echo", &token, &json!({}))).await;
    assert_eq!(after.status, 200, "slots are released when calls finish");
}

#[tokio::test]
async fn a_call_that_waits_on_the_outside_does_not_use_up_the_running_calls() {
    let limits = Limits {
        max_concurrent_calls_per_session: 1,
        max_waiting_calls_per_session: 1,
        ..Limits::default()
    };
    let fx = fixture(config(limits));
    let token = session_token(&fx).await;
    let spawn = |name: &str| {
        let transport = fx.transport.clone();
        let request = call(name, &token, &json!({}));
        tokio::spawn(async move { transport.handle(request).await })
    };
    let waiting = spawn("test.wait");
    bounded(fx.probe.entered.acquire())
        .await
        .expect("entered")
        .forget();
    let other = send(&fx, call("test.echo", &token, &json!({}))).await;
    assert_eq!(other.status, 200, "the only running slot is free");
    let second = send(&fx, call("test.wait", &token, &json!({}))).await;
    assert_eq!(second.status, 429);
    assert_eq!(
        error_code(&second),
        "BUSY",
        "the waiting budget is full too"
    );
    fx.probe.release.add_permits(1);
    assert_eq!(bounded(waiting).await.expect("join").status, 200);
    let again = spawn("test.wait");
    bounded(fx.probe.entered.acquire())
        .await
        .expect("entered")
        .forget();
    fx.probe.release.add_permits(1);
    assert_eq!(
        bounded(again).await.expect("join").status,
        200,
        "the waiting slot is released when the call ends"
    );
}

#[tokio::test]
async fn returning_credit_and_closing_a_stream_need_no_call_slot_but_writing_does() {
    let limits = Limits {
        max_concurrent_calls_per_session: 1,
        ..Limits::default()
    };
    let fx = fixture(config(limits));
    let token = session_token(&fx).await;
    let transport = fx.transport.clone();
    let request = call("test.slow", &token, &json!({}));
    let running = tokio::spawn(async move { transport.handle(request).await });
    bounded(fx.probe.entered.acquire())
        .await
        .expect("entered")
        .forget();
    assert_eq!(
        send(&fx, call("test.echo", &token, &json!({})))
            .await
            .status,
        429,
        "the only slot is taken"
    );
    for (name, args) in [
        ("runtime.stream.ack", json!({"id": 999, "bytes": 1})),
        ("runtime.stream.close", json!({"id": 999})),
        ("runtime.stream.end", json!({"id": 999})),
    ] {
        let response = send(&fx, call(name, &token, &args)).await;
        assert_eq!(
            error_code(&response),
            "NOT_FOUND",
            "{name} reached its handler"
        );
    }
    let write = send(
        &fx,
        call("runtime.stream.write", &token, &json!({"id": 999})),
    )
    .await;
    assert_eq!(
        error_code(&write),
        "BUSY",
        "writing waits for the reader and is counted"
    );
    fx.probe.release.add_permits(1);
    assert_eq!(bounded(running).await.expect("join").status, 200);
}

#[tokio::test]
async fn dropping_the_response_future_aborts_the_handler_and_frees_the_slot() {
    let limits = Limits {
        max_concurrent_calls_per_session: 1,
        ..Limits::default()
    };
    let fx = fixture(config(limits));
    let token = session_token(&fx).await;
    {
        let pending = fx.transport.handle(call("test.hang", &token, &json!({})));
        tokio::pin!(pending);
        tokio::select! {
            _ = &mut pending => panic!("a hanging handler must not finish"),
            entered = fx.probe.entered.acquire() => entered.expect("entered").forget(),
        }
        // `pending` is dropped here: the client went away
    }
    bounded(async {
        while !fx.probe.hang_dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let after = send(&fx, call("test.echo", &token, &json!({}))).await;
    assert_eq!(after.status, 200, "the single call slot is free again");
}

#[tokio::test]
async fn an_oversized_json_body_is_rejected_without_a_handler_call() {
    let limits = Limits {
        max_unary_body: 16,
        ..Limits::default()
    };
    let fx = fixture(config(limits));
    let token = session_token(&fx).await;
    let big = json!({"p": "x".repeat(64)});
    let response = send(&fx, call("test.slow", &token, &big)).await;
    assert_eq!(response.status, 400);
    assert_eq!(error_code(&response), "INVALID_ARGUMENT");
    assert_eq!(
        fx.probe.entered.available_permits(),
        0,
        "the handler must never start"
    );
}

#[test]
fn a_call_waits_for_the_bounded_parser_before_its_handler_runs() {
    // One blocking thread: an admitted parse queues behind the blocker instead of running beside it.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        // A parser slot held by a barrier: while it is held, the parse cannot have run.
        let parse = BlockingExecutor::new(std::num::NonZeroUsize::new(2).expect("non-zero limit"));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let _blocker = parse
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            })
            .expect("blocker admitted");
        bounded(started_rx).await.expect("blocker started");
        let fx = fixture_with_parse(config(Limits::default()), parse);
        let token = session_token(&fx).await;
        let running = {
            let transport = fx.transport.clone();
            let request = call("test.slow", &token, &big_args());
            tokio::spawn(async move { transport.handle(request).await })
        };
        // While the parser is blocked the handler must not start: parsing gates the dispatch.
        // If parsing ever returned to the caller's poll it would run inline and the handler
        // would start without the parser — this bounded loop then fails.
        bounded(async {
            for _ in 0..200 {
                assert_eq!(
                    fx.probe.entered.available_permits(),
                    0,
                    "the handler ran while the parser was still blocked"
                );
                tokio::task::yield_now().await;
            }
        })
        .await;
        release_tx.send(()).expect("parser barrier waiting");
        bounded(fx.probe.entered.acquire())
            .await
            .expect("the handler starts once the parser is free")
            .forget();
        fx.probe.release.add_permits(1);
        assert_eq!(bounded(running).await.expect("join").status, 200);
    });
}

#[test]
fn a_queued_parse_dropped_with_its_call_never_reaches_the_handler() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let parse = BlockingExecutor::new(std::num::NonZeroUsize::new(2).expect("non-zero limit"));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let _blocker = parse
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            })
            .expect("blocker admitted");
        bounded(started_rx).await.expect("blocker started");
        let fx = fixture_with_parse(config(Limits::default()), parse);
        let token = session_token(&fx).await;
        let transport = fx.transport.clone();
        let request = call("test.slow", &token, &big_args());
        let running = tokio::spawn(async move { transport.handle(request).await });
        // While the parser is blocked the call's parse is queued; the handler must not start.
        bounded(async {
            for _ in 0..200 {
                assert_eq!(
                    fx.probe.entered.available_permits(),
                    0,
                    "the handler ran while the parser was still blocked"
                );
                tokio::task::yield_now().await;
            }
        })
        .await;
        // Dropping the call drops its queued parse job with it.
        running.abort();
        release_tx.send(()).expect("parser barrier waiting");
        bounded(async {
            for _ in 0..200 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(
            fx.probe.entered.available_permits(),
            0,
            "the handler ran for an aborted call"
        );
        // The skipped queued job releases its permit only once it runs; retry past a transient
        // parser `BUSY` and require the budget to be usable again.
        let follow = bounded(async {
            loop {
                let response = send(&fx, call("test.echo", &token, &big_args())).await;
                if response.status == 429 && error_code(&response) == "BUSY" {
                    tokio::task::yield_now().await;
                    continue;
                }
                break response;
            }
        })
        .await;
        assert_eq!(follow.status, 200, "the parse budget was not leaked");
    });
}

#[test]
fn small_and_binary_calls_never_meet_a_full_parser() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        // The only parser slot is held: a large JSON body is refused, cheap bodies are not.
        let parse = BlockingExecutor::new(std::num::NonZeroUsize::new(1).expect("non-zero limit"));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let _blocker = parse
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            })
            .expect("blocker admitted");
        bounded(started_rx).await.expect("blocker started");
        let fx = fixture_with_parse(config(Limits::default()), parse);
        let token = session_token(&fx).await;
        let big = send(&fx, call("test.echo", &token, &big_args())).await;
        assert_eq!(error_code(&big), "BUSY", "a large JSON body needs the parser");
        let small = send(&fx, call("test.echo", &token, &json!({"n": 1}))).await;
        assert_eq!(small.status, 200, "a small JSON body is parsed in place");
        assert_eq!(body_json(&small), json!({"n": 1}));
        let binary = request(Method::Post, "call/test.echo")
            .token(&token)
            .octets(Bytes::from(vec![7u8; 1024 * 1024]), &json!({"k": "v"}));
        let binary = send(&fx, binary).await;
        assert_eq!(binary.status, 200, "a binary body is parsed in place");
        assert_eq!(body_json(&binary), json!({"k": "v"}));
        release_tx.send(()).expect("parser barrier waiting");
    });
}

#[tokio::test]
async fn a_stream_reply_is_announced_as_json_with_the_stream_id() {
    let fx = fixture(config(Limits::default()));
    let token = session_token(&fx).await;
    let response = send(&fx, call("test.out", &token, &json!({}))).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert!(body_json(&response)["stream"].is_u64());
}
