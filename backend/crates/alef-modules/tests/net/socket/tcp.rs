// SPDX-License-Identifier: MIT OR Apache-2.0
//! TCP through the registry: connections, listeners, the rights, the stand-in of the user.
use alef_core::security::consent::{Consent, Decision, Right};

use super::*;

#[tokio::test]
async fn a_connection_carries_bytes_both_ways_and_ends_when_both_sides_do() {
    let (server, taken) = echo().await;
    let scope = format!("tcp:127.0.0.1:{}", server.port());
    let app = app(&[&scope]).await;
    let mut conn = connect(&app, "127.0.0.1", server.port(), json!({}))
        .await
        .unwrap();
    assert_eq!(
        conn.remote,
        json!({ "host": "127.0.0.1", "port": server.port() })
    );
    assert_eq!(conn.local["host"], "127.0.0.1");
    assert_ne!(conn.local["port"], 0);

    conn.send(b"hello ").await;
    conn.send(b"world").await;
    assert_eq!(conn.input.exactly(11).await, b"hello world");
    conn.finish();
    assert_eq!(
        conn.input.until_end().await.unwrap(),
        b"",
        "the server ended after the page did, and the stream ends"
    );
    assert_eq!(taken.load(Ordering::SeqCst), 1);

    app.call("socket.close", json!({ "socket": conn.id }))
        .await
        .unwrap();
    let again = app.call("socket.close", json!({ "socket": conn.id })).await;
    assert_eq!(code(again), ErrorCode::NotFound, "a socket is closed once");
}

#[tokio::test]
async fn a_lot_of_bytes_go_both_ways_at_once_and_arrive_whole() {
    let (server, _) = echo().await;
    let scope = format!("tcp:127.0.0.1:{}", server.port());
    let app = app(&[&scope]).await;
    let mut conn = connect(&app, "127.0.0.1", server.port(), json!({}))
        .await
        .unwrap();
    let data = pattern(4 * 1024 * 1024);
    let writer = conn.output.take().unwrap();
    let sending = data.clone();
    let task = tokio::spawn(async move {
        for piece in sending.chunks(64 * 1024) {
            writer.write(Bytes::copy_from_slice(piece)).await.unwrap();
        }
        writer.end();
    });
    let mut got = Vec::new();
    while got.len() < data.len() {
        got.extend(conn.input.exactly(1).await);
        let more = conn.input.pending.len();
        got.extend(conn.input.exactly(more).await);
    }
    task.await.unwrap();
    assert_eq!(got.len(), data.len());
    assert!(got == data, "the bytes came back whole and in order");
}

#[tokio::test]
async fn an_address_outside_the_scope_is_denied_before_anything_is_tried() {
    let (allowed, _) = echo().await;
    let (other, taken) = echo().await;
    let scope = format!("tcp:127.0.0.1:{}", allowed.port());
    let app = app(&[&scope]).await;
    for (host, port) in [
        ("127.0.0.1", other.port()),
        ("localhost", allowed.port()),
        ("127.0.0.1", 0),
    ] {
        let error = connect(&app, host, port, json!({})).await.err().unwrap();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{host}:{port}");
        assert_eq!(error.details, Some(json!({ "permission": "net.socket" })));
    }
    assert_eq!(taken.load(Ordering::SeqCst), 0);
    let none = crate::common::Fixture::new(None, &[]).await;
    let denied = connect(&none, "127.0.0.1", allowed.port(), json!({})).await;
    assert_eq!(code(denied), ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn a_server_that_is_not_there_is_the_network() {
    let port = free_port().await;
    let scope = format!("tcp:127.0.0.1:{port}");
    let app = app(&[&scope]).await;
    let error = connect(&app, "127.0.0.1", port, json!({}))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::Network);
    assert!(
        error.message.starts_with("cannot connect to the server"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn a_server_that_closes_ends_the_stream_and_a_close_by_the_page_closes_the_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen, mut heard) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        // The first connection gets a word and is closed; the second is held until the page closes it.
        let (mut first, _) = listener.accept().await.unwrap();
        first.write_all(b"bye").await.unwrap();
        drop(first);
        let (mut second, _) = listener.accept().await.unwrap();
        let mut rest = Vec::new();
        let _ = second.read_to_end(&mut rest).await;
        let _ = seen.send(rest);
    });
    let scope = format!("tcp:127.0.0.1:{port}");
    let app = app(&[&scope]).await;

    let mut first = connect(&app, "127.0.0.1", port, json!({})).await.unwrap();
    assert_eq!(first.input.exactly(3).await, b"bye");
    assert_eq!(first.input.until_end().await.unwrap(), b"");

    let second = connect(&app, "127.0.0.1", port, json!({})).await.unwrap();
    second.send(b"before the close").await;
    let closing = std::time::Instant::now();
    app.call("socket.close", json!({ "socket": second.id }))
        .await
        .unwrap();
    assert!(
        closing.elapsed() < Duration::from_secs(1),
        "a close does not wait for the time it has to write: {:?}",
        closing.elapsed()
    );
    let rest = tokio::time::timeout(Duration::from_secs(10), heard.recv())
        .await
        .expect("the server saw the end of the connection")
        .unwrap();
    assert_eq!(
        rest, b"before the close",
        "what was sent arrived, then the end"
    );
}

#[tokio::test]
async fn a_listener_hands_each_connection_to_the_page_as_a_frame() {
    let app = app(&["listen:127.0.0.1:*"]).await;
    let listening = app
        .call("socket.listen", json!({ "port": 0 }))
        .await
        .unwrap();
    let port = listening["localAddress"]["port"].as_u64().unwrap() as u16;
    assert_eq!(listening["localAddress"]["host"], "127.0.0.1");
    assert_ne!(port, 0, "the port the system chose");
    let mut accepted = Pipe::open(&app, listening["accept"].as_u64().unwrap());

    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let from = client.local_addr().unwrap().port();
    let frame = accepted.json().await.expect("a connection");
    let mut conn = Conn::of(&app, &frame);
    assert_eq!(conn.remote, json!({ "host": "127.0.0.1", "port": from }));
    assert_eq!(conn.local, json!({ "host": "127.0.0.1", "port": port }));

    client.write_all(b"ping").await.unwrap();
    assert_eq!(conn.input.exactly(4).await, b"ping");
    conn.send(b"pong").await;
    let mut answer = [0_u8; 4];
    client.read_exact(&mut answer).await.unwrap();
    assert_eq!(&answer, b"pong");

    let mut second = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let other = Conn::of(&app, &accepted.json().await.expect("a second connection"));
    assert_ne!(
        other.id, conn.id,
        "each connection is a resource of its own"
    );

    // A listener that is closed takes nobody more, and the connections it gave stay.
    app.call("socket.close", json!({ "socket": listening["server"] }))
        .await
        .unwrap();
    let mut refused = false;
    for _ in 0..40 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(refused, "a closed listener still took a connection");
    client.write_all(b"again").await.unwrap();
    assert_eq!(conn.input.exactly(5).await, b"again");
    conn.finish();
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut rest))
        .await
        .expect("the end of the page was the end for the client")
        .unwrap();
    assert!(rest.is_empty());
    second.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_listener_needs_the_right_to_take_the_port() {
    let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let busy = taken.local_addr().unwrap().port();
    let pinned = free_port().await;
    let scope = format!("listen:127.0.0.1:{pinned}");
    let app = app(&[&scope]).await;
    for args in [
        json!({ "host": "0.0.0.0", "port": pinned }),
        json!({ "port": pinned + 1 }),
        json!({ "port": 0 }),
    ] {
        let denied = app.call("socket.listen", args.clone()).await;
        assert_eq!(code(denied), ErrorCode::PermissionDenied, "{args}");
    }
    let wide = crate::common::Fixture::new(Some(&manifest(&["listen:127.0.0.1:*"])), &[]).await;
    let in_use = wide.call("socket.listen", json!({ "port": busy })).await;
    let error = in_use.expect_err("the port is taken");
    assert_ne!(error.code, ErrorCode::PermissionDenied, "{}", error.message);
    let ok = app.call("socket.listen", json!({ "port": pinned })).await;
    assert!(ok.is_ok(), "{ok:?}");
}

#[tokio::test]
async fn what_the_user_substituted_is_a_dead_network() {
    let (server, taken) = echo().await;
    let scope = format!("tcp:127.0.0.1:{}", server.port());
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("net.socket", &scope), Decision::Substitute);
    let app = crate::common::Fixture::new(Some(&manifest(&[&scope])), &[])
        .await
        .with_consent(consent);
    let started = std::time::Instant::now();
    let dead = connect(
        &app,
        "127.0.0.1",
        server.port(),
        json!({ "timeoutMs": 300 }),
    )
    .await;
    assert_eq!(code(dead), ErrorCode::Timeout);
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        taken.load(Ordering::SeqCst),
        0,
        "nothing reached the server"
    );

    // A port nobody took: the page gets an address, and nobody comes to it.
    let port = free_port().await;
    let listen = format!("listen:127.0.0.1:{port}");
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("net.socket", &listen), Decision::Substitute);
    let app = crate::common::Fixture::new(Some(&manifest(&[&listen])), &[])
        .await
        .with_consent(consent);
    let reply = app
        .call("socket.listen", json!({ "port": port }))
        .await
        .unwrap();
    assert_eq!(
        reply["localAddress"],
        json!({ "host": "127.0.0.1", "port": port })
    );
    let mut accepted = Pipe::open(&app, reply["accept"].as_u64().unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(300), accepted.frame())
            .await
            .is_err(),
        "the stand-in waits for connections and does not fail"
    );
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err());
    app.call("socket.close", json!({ "socket": reply["server"] }))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_commands_take_only_what_is_theirs() {
    let (server, _) = echo().await;
    let scope = format!("tcp:127.0.0.1:{}", server.port());
    let datagrams = format!("udp:127.0.0.1:{}", server.port());
    let app = app(&[&scope, &datagrams]).await;
    let missing = app.call("socket.close", json!({ "socket": 4242 })).await;
    assert_eq!(code(missing), ErrorCode::NotFound);
    let unknown = connect(&app, "127.0.0.1", server.port(), json!({ "verify": false })).await;
    assert_eq!(code(unknown), ErrorCode::InvalidArgument);
    let conn = connect(&app, "127.0.0.1", server.port(), json!({}))
        .await
        .unwrap();
    let not_udp = app
        .call_reply(
            "socket.send",
            json!({ "socket": conn.id, "host": "127.0.0.1", "port": server.port() }),
            Some(Bytes::from_static(b"x")),
        )
        .await;
    assert_eq!(
        code(not_udp),
        ErrorCode::InvalidArgument,
        "a connection is no socket of UDP"
    );
}

#[tokio::test]
async fn a_close_takes_only_a_socket_and_nothing_that_another_module_made() {
    let server = crate::server::Server::start().await;
    let app = Fixture::new(Some(&manifest_with_http(&[], &[server.scope()])), &[]).await;
    let started = app
        .call("http.start", json!({ "url": server.url("/hello") }))
        .await
        .unwrap();
    let closing = app
        .call("socket.close", json!({ "socket": started["request"] }))
        .await;
    assert_eq!(code(closing), ErrorCode::NotFound);
    app.session()
        .streams()
        .incoming_writer(StreamId(started["upload"].as_u64().unwrap()))
        .expect("an incoming stream")
        .end();
    let head = app
        .call("http.response", json!({ "request": started["request"] }))
        .await
        .unwrap();
    assert_eq!(head["status"], 200, "the request is still there");
}

#[tokio::test]
async fn a_connection_that_is_reset_fails_its_stream_with_the_network() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        stream.set_zero_linger().unwrap();
        drop(stream);
    });
    let scope = format!("tcp:127.0.0.1:{port}");
    let app = app(&[&scope]).await;
    let mut conn = connect(&app, "127.0.0.1", port, json!({})).await.unwrap();
    let ended = conn.input.until_end().await.unwrap_err();
    assert_eq!(ended.code, ErrorCode::Network);
    assert!(
        ended.message.starts_with("the connection broke"),
        "{}",
        ended.message
    );
}

#[tokio::test]
async fn an_output_the_page_aborts_ends_for_the_peer_after_what_it_sent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen, mut heard) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut all = Vec::new();
        let _ = stream.read_to_end(&mut all).await;
        let _ = seen.send(all);
    });
    let scope = format!("tcp:127.0.0.1:{port}");
    let app = app(&[&scope]).await;
    let mut conn = connect(&app, "127.0.0.1", port, json!({})).await.unwrap();
    conn.send(b"data").await;
    conn.output
        .take()
        .unwrap()
        .abort(AlefError::new(ErrorCode::Closed, "the page gave up"));
    let all = tokio::time::timeout(Duration::from_secs(10), heard.recv())
        .await
        .expect("the peer saw the end")
        .unwrap();
    assert_eq!(all, b"data");
}

#[tokio::test]
async fn a_close_writes_what_the_runtime_took_though_the_peer_was_slow_to_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (go, wait) = tokio::sync::oneshot::channel::<()>();
    let (seen, mut heard) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        // Nothing is read until the page has closed the socket.
        let _ = wait.await;
        let mut total = 0_usize;
        let mut buffer = vec![0_u8; 64 * 1024];
        while let Ok(count) = stream.read(&mut buffer).await {
            if count == 0 {
                break;
            }
            total += count;
        }
        let _ = seen.send(total);
    });
    let scope = format!("tcp:127.0.0.1:{port}");
    let app = app(&[&scope]).await;
    let mut conn = connect(&app, "127.0.0.1", port, json!({})).await.unwrap();
    let output = conn.output.take().unwrap();
    let taken = Arc::new(AtomicUsize::new(0));
    let counting = taken.clone();
    tokio::spawn(async move {
        let piece = Bytes::from(vec![7_u8; 64 * 1024]);
        while output.write(piece.clone()).await.is_ok() {
            counting.fetch_add(1, Ordering::SeqCst);
        }
    });
    // The peer reads nothing, so the buffers fill and the page cannot send any more.
    let mut before = usize::MAX;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = taken.load(Ordering::SeqCst);
        if now == before {
            break;
        }
        before = now;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = go.send(());
    });
    app.call("socket.close", json!({ "socket": conn.id }))
        .await
        .unwrap();
    let total = tokio::time::timeout(Duration::from_secs(20), heard.recv())
        .await
        .expect("the peer saw the end")
        .unwrap();
    assert_eq!(total % (64 * 1024), 0, "whole pieces");
    assert!(
        total >= before * 64 * 1024,
        "{} pieces were taken, {} arrived",
        before,
        total / (64 * 1024)
    );
}
