// SPDX-License-Identifier: MIT OR Apache-2.0
//! UDP through the registry: datagrams out and in, the rights each way, the stand-in of the user.
use alef_core::security::consent::{Consent, Decision, Right};
use base64::{engine::general_purpose::STANDARD, Engine};
use tokio::net::UdpSocket;

use super::*;

/// A UDP socket of the page, bound on the loopback by the runtime.
struct Bound {
    id: u64,
    port: u16,
    messages: Pipe,
}

async fn bind(app: &Fixture) -> Bound {
    let reply = app.call("socket.udp", json!({})).await.unwrap();
    assert_eq!(reply["localAddress"]["host"], "127.0.0.1");
    Bound {
        id: reply["socket"].as_u64().unwrap(),
        port: reply["localAddress"]["port"].as_u64().unwrap() as u16,
        messages: Pipe::open(app, reply["messages"].as_u64().unwrap()),
    }
}

async fn send(app: &Fixture, socket: u64, to: u16, data: &[u8]) -> Result<Value, AlefError> {
    app.call_reply(
        "socket.send",
        json!({ "socket": socket, "host": "127.0.0.1", "port": to }),
        Some(Bytes::copy_from_slice(data)),
    )
    .await
    .map(|reply| match reply {
        alef_core::registry::command::Reply::Json(value) => value,
        other => panic!("expected JSON, got {other:?}"),
    })
}

async fn peer() -> (UdpSocket, u16) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    (socket, port)
}

#[tokio::test]
async fn datagrams_go_out_and_come_in_with_their_senders() {
    let (peer, peer_port) = peer().await;
    let datagrams = format!("udp:127.0.0.1:{peer_port}");
    let app = app(&["listen:127.0.0.1:*", &datagrams]).await;
    let mut page = bind(&app).await;
    assert_ne!(page.port, 0);

    send(&app, page.id, peer_port, b"ping").await.unwrap();
    let mut buffer = vec![0_u8; 70_000];
    let (count, from) = tokio::time::timeout(Duration::from_secs(10), peer.recv_from(&mut buffer))
        .await
        .expect("the datagram arrived")
        .unwrap();
    assert_eq!(&buffer[..count], b"ping");
    assert_eq!(from.port(), page.port);

    peer.send_to(b"pong", ("127.0.0.1", page.port))
        .await
        .unwrap();
    let datagram = page.messages.json().await.expect("a datagram");
    assert_eq!(datagram["host"], "127.0.0.1");
    assert_eq!(datagram["port"], peer_port);
    assert_eq!(
        STANDARD.decode(datagram["data"].as_str().unwrap()).unwrap(),
        b"pong"
    );

    // An empty datagram is a datagram, in both directions.
    peer.send_to(b"", ("127.0.0.1", page.port)).await.unwrap();
    let empty = page.messages.json().await.expect("an empty datagram");
    assert_eq!(empty["data"], "");
    send(&app, page.id, peer_port, b"").await.unwrap();
    let (count, _) = tokio::time::timeout(Duration::from_secs(10), peer.recv_from(&mut buffer))
        .await
        .expect("the empty datagram arrived")
        .unwrap();
    assert_eq!(count, 0);

    // A big datagram goes and comes back whole (macOS takes 9216 bytes by default, so no more here).
    let big = pattern(8_000);
    send(&app, page.id, peer_port, &big).await.unwrap();
    let (count, _) = tokio::time::timeout(Duration::from_secs(10), peer.recv_from(&mut buffer))
        .await
        .expect("the big datagram arrived")
        .unwrap();
    assert!(buffer[..count] == big[..]);
    peer.send_to(&big, ("127.0.0.1", page.port)).await.unwrap();
    let back = page.messages.json().await.expect("the big datagram");
    assert!(STANDARD.decode(back["data"].as_str().unwrap()).unwrap() == big);

    app.call("socket.close", json!({ "socket": page.id }))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_datagram_goes_only_where_udp_lets_it_and_a_port_is_taken_only_as_listen_lets_it() {
    let (allowed, allowed_port) = peer().await;
    let (other, other_port) = peer().await;
    let datagrams = format!("udp:127.0.0.1:{allowed_port}");
    let app = app(&["listen:127.0.0.1:*", &datagrams]).await;
    let page = bind(&app).await;

    let denied = send(&app, page.id, other_port, b"secret").await;
    assert_eq!(code(denied), ErrorCode::PermissionDenied);
    let mut buffer = [0_u8; 16];
    assert!(
        tokio::time::timeout(Duration::from_millis(300), other.recv_from(&mut buffer))
            .await
            .is_err(),
        "nothing was sent to the place outside the scope"
    );
    send(&app, page.id, allowed_port, b"fine").await.unwrap();
    let (count, _) = tokio::time::timeout(Duration::from_secs(10), allowed.recv_from(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buffer[..count], b"fine");

    for args in [
        json!({ "host": "0.0.0.0" }),
        json!({ "host": "127.0.0.2", "port": 5000 }),
    ] {
        let taken = app.call("socket.udp", args.clone()).await;
        assert_eq!(code(taken), ErrorCode::PermissionDenied, "{args}");
    }
    let only_send = crate::common::Fixture::new(Some(&manifest(&[&datagrams])), &[]).await;
    let no_port = only_send.call("socket.udp", json!({})).await;
    assert_eq!(
        code(no_port),
        ErrorCode::PermissionDenied,
        "to send is not to take a port"
    );
}

#[tokio::test]
async fn a_datagram_has_a_limit_and_goes_through_a_socket_of_udp_only() {
    let (_peer, peer_port) = peer().await;
    let datagrams = format!("udp:127.0.0.1:{peer_port}");
    let app = app(&["listen:127.0.0.1:*", &datagrams]).await;
    let page = bind(&app).await;
    let too_big = send(&app, page.id, peer_port, &vec![0_u8; 65_508]).await;
    assert_eq!(code(too_big), ErrorCode::InvalidArgument);
    let missing = send(&app, 4242, peer_port, b"x").await;
    assert_eq!(code(missing), ErrorCode::NotFound);
    app.call("socket.close", json!({ "socket": page.id }))
        .await
        .unwrap();
    let closed = send(&app, page.id, peer_port, b"x").await;
    assert_eq!(
        code(closed),
        ErrorCode::NotFound,
        "a closed socket sends nothing"
    );
    let unknown = app.call("socket.udp", json!({ "reuse": true })).await;
    assert_eq!(code(unknown), ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn what_the_user_substituted_goes_nowhere_and_nothing_comes() {
    let (peer, peer_port) = peer().await;
    let datagrams = format!("udp:127.0.0.1:{peer_port}");

    // The datagrams the page sends are dropped, though the socket is real.
    let mut consent = Consent::undecided();
    consent.set(
        Right::scoped("net.socket", &datagrams),
        Decision::Substitute,
    );
    consent.set(
        Right::scoped("net.socket", "listen:127.0.0.1:*"),
        Decision::Allow,
    );
    let app =
        crate::common::Fixture::new(Some(&manifest(&["listen:127.0.0.1:*", &datagrams])), &[])
            .await
            .with_consent(consent);
    let page = bind(&app).await;
    send(&app, page.id, peer_port, b"lost").await.unwrap();
    let mut buffer = [0_u8; 16];
    assert!(
        tokio::time::timeout(Duration::from_millis(300), peer.recv_from(&mut buffer))
            .await
            .is_err(),
        "a datagram the user substituted was sent"
    );

    // A port nobody took: it can be sent through, nothing is sent, and nothing comes.
    let mut consent = Consent::undecided();
    consent.set(
        Right::scoped("net.socket", "listen:127.0.0.1:*"),
        Decision::Substitute,
    );
    consent.set(Right::scoped("net.socket", &datagrams), Decision::Allow);
    let app =
        crate::common::Fixture::new(Some(&manifest(&["listen:127.0.0.1:*", &datagrams])), &[])
            .await
            .with_consent(consent);
    let standing = app.call("socket.udp", json!({})).await.unwrap();
    let id = standing["socket"].as_u64().unwrap();
    let mut messages = Pipe::open(&app, standing["messages"].as_u64().unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(300), messages.frame())
            .await
            .is_err(),
        "the stand-in waits for datagrams and does not fail"
    );
    send(&app, id, peer_port, b"lost").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), peer.recv_from(&mut buffer))
            .await
            .is_err(),
        "a stand-in sent a datagram"
    );
    app.call("socket.close", json!({ "socket": id }))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_datagram_that_found_nobody_does_not_stop_the_socket() {
    let (gone, gone_port) = peer().await;
    drop(gone);
    let (live, _) = peer().await;
    let app = app(&["listen:127.0.0.1:*", "udp:127.0.0.1:*"]).await;
    let mut page = bind(&app).await;
    for _ in 0..30 {
        send(&app, page.id, gone_port, b"to nobody").await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    live.send_to(b"hello", ("127.0.0.1", page.port))
        .await
        .unwrap();
    let datagram = page.messages.json().await.expect("the socket goes on");
    assert_eq!(
        STANDARD.decode(datagram["data"].as_str().unwrap()).unwrap(),
        b"hello"
    );
}
