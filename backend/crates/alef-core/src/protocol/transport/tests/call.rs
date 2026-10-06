// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unary calls: bodies, limits, permissions, panics, concurrency and client aborts.
use super::*;

async fn session_token(fx: &Fixture) -> String {
    open(fx, WINDOW).await.token().to_owned()
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
async fn a_stream_reply_is_announced_as_json_with_the_stream_id() {
    let fx = fixture(config(Limits::default()));
    let token = session_token(&fx).await;
    let response = send(&fx, call("test.out", &token, &json!({}))).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert!(body_json(&response)["stream"].is_u64());
}
