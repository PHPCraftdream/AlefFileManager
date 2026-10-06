// SPDX-License-Identifier: MIT OR Apache-2.0
//! Authentication, handshake, origin policy and routing.
use super::*;

#[tokio::test]
async fn every_bad_credential_gets_the_same_denial_and_never_reaches_the_handler() {
    let fx = fixture(config(Limits::default()));
    let own = open(&fx, 1).await;
    let other = open(&fx, 2).await;
    let stale = open(&fx, 3).await.token().to_owned();
    open(&fx, 3).await;
    let args = json!({"path": "x"});
    let plain = || request(Method::Post, "call/test.denied").json(&args);
    let cases = [
        ("no header", plain()),
        (
            "wrong scheme",
            plain().header_set("authorization", "Basic abc"),
        ),
        ("no token", plain().header_set("authorization", "Bearer")),
        (
            "empty token",
            plain().header_set("authorization", "Bearer "),
        ),
        ("unknown token", plain().token("wrong")),
        ("token of another window", plain().token(other.token())),
        (
            "stale token after reload",
            plain().token(&stale).on_window(3),
        ),
        ("bootstrap used as session token", plain().token(BOOTSTRAP)),
    ];
    for (what, case) in cases {
        assert_denied(&send(&fx, case).await, what);
    }
    assert_eq!(fx.probe.handled.load(Ordering::SeqCst), 0);
    let control = send(&fx, call("test.echo", own.token(), &json!({"ok": true}))).await;
    assert_eq!(control.status, 200, "control: the right token works");
}

#[tokio::test]
async fn hello_hands_the_current_document_token_only_to_the_bootstrap_holder() {
    let fx = fixture(config(Limits::default()));
    let hello = || request(Method::Post, "call/runtime.hello").token(BOOTSTRAP);
    let none = send(&fx, hello()).await;
    assert_eq!(none.status, 404, "no document session yet");
    let session = open(&fx, 1).await;
    let first = send(&fx, hello()).await;
    assert_eq!(first.status, 200);
    let info = body_json(&first);
    assert_eq!(info["protocol"], 1);
    assert_eq!(info["runtime"], "9.9.9");
    assert_eq!(info["platform"], std::env::consts::OS);
    assert_eq!(info["arch"], std::env::consts::ARCH);
    assert_eq!(info["modules"], json!(["runtime", "test"]));
    assert_eq!(info["token"], session.token());
    assert_eq!(info["limits"]["chunkSize"], 256 * 1024);
    assert_eq!(info["limits"]["streamWindow"], 1024 * 1024);
    let again = send(&fx, hello()).await;
    assert_eq!(
        body_json(&again)["token"],
        session.token(),
        "same document, same token"
    );
    assert!(!String::from_utf8_lossy(body(&first)).contains(BOOTSTRAP));
    let bad = [
        ("missing", request(Method::Post, "call/runtime.hello")),
        (
            "wrong",
            request(Method::Post, "call/runtime.hello").token("nope"),
        ),
        (
            "session token",
            request(Method::Post, "call/runtime.hello").token(session.token()),
        ),
    ];
    for (what, bad) in bad {
        assert_denied(&send(&fx, bad).await, what);
    }
    let reloaded = open(&fx, 1).await;
    let fresh = body_json(&send(&fx, hello()).await);
    assert_eq!(fresh["token"], reloaded.token());
    assert_ne!(fresh["token"], info["token"], "reload issues a new token");
    let old = send(&fx, call("test.echo", session.token(), &json!({}))).await;
    assert_denied(&old, "old token after reload");
}

#[tokio::test]
async fn origin_allow_list_is_enforced_and_echoed() {
    let mut cfg = config(Limits::default());
    cfg.allowed_origins = vec!["https://app.alef".into()];
    let fx = fixture(cfg);
    let session = open(&fx, 1).await;
    let with_origin =
        |origin: &str| call("test.echo", session.token(), &json!({})).header_set("origin", origin);
    let ok = send(&fx, with_origin("https://app.alef")).await;
    assert_eq!(ok.status, 200);
    assert_eq!(
        ok.header("access-control-allow-origin"),
        Some("https://app.alef")
    );
    assert_eq!(ok.header("vary"), Some("origin"));
    assert_eq!(ok.header("cache-control"), Some("no-store"));
    assert_eq!(ok.header("x-content-type-options"), Some("nosniff"));
    let wrong = send(&fx, with_origin("https://evil.example")).await;
    assert_denied(&wrong, "wrong origin");
    assert_eq!(wrong.header("access-control-allow-origin"), None);
    let missing = send(&fx, call("test.echo", session.token(), &json!({}))).await;
    assert_denied(&missing, "no origin while a list is configured");
    let hello = request(Method::Post, "call/runtime.hello")
        .token(BOOTSTRAP)
        .header_set("origin", "https://evil.example");
    assert_denied(&send(&fx, hello).await, "hello from a foreign origin");
    let preflight =
        request(Method::Options, "call/test.echo").header_set("origin", "https://app.alef");
    let allowed = send(&fx, preflight).await;
    assert_eq!(allowed.status, 204);
    assert_eq!(
        allowed.header("access-control-allow-origin"),
        Some("https://app.alef")
    );
    assert_eq!(
        allowed.header("access-control-allow-methods"),
        Some("GET, POST, OPTIONS")
    );
    assert_eq!(
        allowed.header("access-control-allow-headers"),
        Some("authorization, content-type, x-alef-args")
    );
    assert_eq!(allowed.header("access-control-max-age"), Some("600"));
    let foreign = request(Method::Options, "stream/1").header_set("origin", "https://evil.example");
    let refused = send(&fx, foreign).await;
    assert_denied(&refused, "foreign preflight");
    assert_eq!(refused.header("access-control-allow-origin"), None);
}

#[tokio::test]
async fn without_an_allow_list_any_origin_is_accepted_and_echoed() {
    let fx = fixture(config(Limits::default()));
    let session = open(&fx, 1).await;
    let request = call("test.echo", session.token(), &json!({}))
        .header_set("origin", "http://127.0.0.1:3000");
    let response = send(&fx, request).await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("access-control-allow-origin"),
        Some("http://127.0.0.1:3000")
    );
    let plain = send(&fx, call("test.echo", session.token(), &json!({}))).await;
    assert_eq!(
        plain.header("access-control-allow-origin"),
        None,
        "nothing to echo"
    );
}

#[tokio::test]
async fn routes_methods_and_command_names_are_validated() {
    let fx = fixture(config(Limits::default()));
    let session = open(&fx, 1).await;
    let token = session.token();
    let get_call = request(Method::Get, "call/test.echo").token(token);
    assert_eq!(send(&fx, get_call).await.status, 405);
    let post_stream = request(Method::Post, "stream/1").token(token);
    assert_eq!(send(&fx, post_stream).await.status, 405);
    for path in ["", "nothing", "calls/test.echo", "stream", "call"] {
        let unknown = request(Method::Post, path).token(token);
        assert_eq!(send(&fx, unknown).await.status, 404, "{path:?}");
    }
    assert_eq!(
        send(&fx, request(Method::Options, "nothing")).await.status,
        404
    );
    for name in [
        "Bad",
        "a..b",
        "%41.x",
        "",
        "test",
        ".test.echo",
        "test.echo.",
        "../x.y",
        "runtime.unknown",
        "test.missing",
    ] {
        let response = send(&fx, call(name, token, &json!({}))).await;
        assert_eq!(response.status, 404, "{name:?}");
    }
    let leading = request(Method::Post, "/call/test.echo")
        .token(token)
        .json(&json!({"a": 1}));
    assert_eq!(
        send(&fx, leading).await.status,
        200,
        "a leading slash is tolerated"
    );
}

#[tokio::test]
async fn secrets_never_appear_in_debug_output_or_error_responses() {
    let fx = fixture(config(Limits::default()));
    let session = open(&fx, 1).await;
    let printed = format!("{:?}", config(Limits::default()));
    assert!(!printed.contains(BOOTSTRAP), "{printed}");
    assert!(!format!("{session:?}").contains(session.token()));
    let presented = "presented-secret-token-xyz";
    let denied = send(&fx, call("test.echo", presented, &json!({}))).await;
    assert!(!format!("{denied:?}").contains(presented));
    let broken = request(Method::Post, "call/test.echo")
        .token(session.token())
        .header_set("content-type", "application/octet-stream")
        .header_set("x-alef-args", "%zz");
    let malformed = send(&fx, broken).await;
    assert_eq!(malformed.status, 400);
    assert!(!format!("{malformed:?}").contains(session.token()));
}
