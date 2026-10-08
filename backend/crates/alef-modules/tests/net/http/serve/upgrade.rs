// SPDX-License-Identifier: MIT OR Apache-2.0
//! The offer of a WebSocket to the server of the page: what the page is told of it, the connection the page
//! is handed when it takes the offer (messages each way, a close from either side, a big message), the
//! offers the page may not take, the Origin, TLS, and a connection that outlives the server that took it.
use alef_core::protocol::frame::Frame;
use async_tungstenite::{
    tokio::{client_async, TokioAdapter},
    tungstenite::{
        handshake::client::Response as Handshake,
        protocol::{
            frame::{
                coding::{CloseCode, Data as OpData, OpCode},
                Frame as WsFrame,
            },
            CloseFrame,
        },
        ClientRequestBuilder, Error as WsError, Message,
    },
    WebSocketStream,
};
use futures_util::StreamExt;
use tokio::task::JoinHandle;

use super::*;
use crate::websocket::{event, pattern, Event};

type Peer = WebSocketStream<TokioAdapter<TcpStream>>;
type Joining = JoinHandle<Result<(Peer, Handshake), WsError>>;

/// A client of WebSocket on the loopback, with the headers and the subprotocols it offers.
async fn dial(port: u16, path: &str, headers: &[(&str, String)], offered: &[&str]) -> Joining {
    let uri = format!("ws://127.0.0.1:{port}{path}").parse().unwrap();
    let mut builder = ClientRequestBuilder::new(uri);
    for (name, value) in headers {
        builder = builder.with_header(*name, value.clone());
    }
    for protocol in offered {
        builder = builder.with_sub_protocol(*protocol);
    }
    tokio::spawn(async move {
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client_async(builder, tcp).await
    })
}

/// The page takes the offer of the request: the connection it is handed, and the stream of its messages.
async fn take(
    app: &Fixture,
    request: u64,
    protocol: Option<&str>,
) -> Result<(u64, Pipe), AlefError> {
    let mut args = json!({ "request": request });
    if let Some(protocol) = protocol {
        args["protocol"] = json!(protocol);
    }
    let reply = app.call("http.upgrade", args).await?;
    assert_eq!(reply["protocol"], protocol.unwrap_or(""));
    Ok((
        reply["socket"].as_u64().unwrap(),
        Pipe::open(app, reply["messages"].as_u64().unwrap()),
    ))
}

async fn send(app: &Fixture, socket: u64, text: bool, data: &[u8]) {
    let reply = app
        .call_reply(
            "websocket.send",
            json!({ "socket": socket, "text": text }),
            Some(Bytes::copy_from_slice(data)),
        )
        .await;
    assert!(reply.is_ok(), "{reply:?}");
}

/// The next message of the client, a control frame apart.
async fn heard(peer: &mut Peer) -> Message {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(20), peer.next())
            .await
            .expect("the page sent nothing")
            .expect("the connection ended")
            .expect("the connection broke");
        if !matches!(message, Message::Ping(_) | Message::Pong(_)) {
            return message;
        }
    }
}

/// What the client sees until the connection ends: it answers a close by being read.
async fn until_end(mut peer: Peer) -> Vec<Message> {
    let mut seen = Vec::new();
    while let Ok(Some(Ok(message))) =
        tokio::time::timeout(Duration::from_secs(20), peer.next()).await
    {
        seen.push(message);
    }
    seen
}

/// Whether the connection ends for the client in a few seconds, whichever way.
async fn ends(mut peer: Peer) -> bool {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(_)) = peer.next().await {}
    })
    .await
    .is_ok()
}

fn refused(joined: Result<(Peer, Handshake), WsError>) -> u16 {
    match joined {
        Err(WsError::Http(response)) => response.status().as_u16(),
        Err(other) => panic!("another failure: {other}"),
        Ok(_) => panic!("the handshake was taken"),
    }
}

#[tokio::test]
async fn an_offer_reaches_the_page_and_taking_it_gives_a_connection_for_messages_both_ways() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    let origin = format!("http://127.0.0.1:{}", served.port);
    let joining = dial(
        served.port,
        "/chat?x=1",
        &[("origin", origin)],
        &["chat", "superchat"],
    )
    .await;
    let seen = next_request(&mut served.requests).await.expect("the offer");
    assert!(seen.upgrade, "the page is told it is an offer");
    assert_eq!(seen.protocols, ["chat", "superchat"]);
    assert_eq!(
        (seen.method.as_str(), seen.url.as_str()),
        ("GET", "/chat?x=1")
    );
    let (socket, mut messages) = take(&app, seen.id, Some("superchat")).await.unwrap();
    let (mut peer, handshake) = joining.await.unwrap().unwrap();
    assert_eq!(handshake.status(), 101);
    assert_eq!(handshake.headers()["sec-websocket-protocol"], "superchat");

    let text = "héllo — мир 🌍";
    peer.send(Message::text(text)).await.unwrap();
    peer.send(Message::binary(vec![0, 255, 7])).await.unwrap();
    peer.send(Message::text("")).await.unwrap();
    assert_eq!(
        event(&mut messages).await,
        Some(Event::Text(text.to_owned()))
    );
    assert_eq!(
        event(&mut messages).await,
        Some(Event::Binary(vec![0, 255, 7]))
    );
    assert_eq!(event(&mut messages).await, Some(Event::Text(String::new())));

    send(&app, socket, true, "back".as_bytes()).await;
    send(&app, socket, false, &[9, 8, 7]).await;
    assert_eq!(heard(&mut peer).await, Message::text("back"));
    assert_eq!(heard(&mut peer).await, Message::binary(vec![9, 8, 7]));

    // The page closes; the client answers by being read, and the page hears its answer.
    let (closed, seen_by_peer) = tokio::join!(
        app.call(
            "websocket.close",
            json!({ "socket": socket, "code": 4000, "reason": "done" })
        ),
        until_end(peer)
    );
    closed.unwrap();
    assert!(
        seen_by_peer
            .iter()
            .any(|message| matches!(message, Message::Close(Some(frame))
            if u16::from(frame.code) == 4000 && frame.reason == "done")),
        "{seen_by_peer:?}"
    );
    assert_eq!(
        event(&mut messages).await,
        Some(Event::Close {
            code: 4000,
            reason: "done".to_owned(),
            clean: true
        })
    );
}

#[tokio::test]
async fn a_big_message_goes_each_way_and_a_close_of_the_client_is_heard() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    let joining = dial(served.port, "/", &[], &[]).await;
    let seen = next_request(&mut served.requests).await.unwrap();
    assert!(seen.upgrade);
    assert!(seen.protocols.is_empty());
    let (socket, mut messages) = take(&app, seen.id, None).await.unwrap();
    let (mut peer, handshake) = joining.await.unwrap().unwrap();
    assert!(handshake.headers().get("sec-websocket-protocol").is_none());

    let big = pattern(BIG);
    peer.send(Message::binary(big.clone())).await.unwrap();
    let Some(Event::Binary(got)) = event(&mut messages).await else {
        panic!("a binary message came");
    };
    assert!(got == big, "the message came whole and in order");
    let down = pattern(250 * 1024);
    send(&app, socket, false, &down).await;
    assert_eq!(heard(&mut peer).await, Message::binary(down));

    peer.close(Some(CloseFrame {
        code: CloseCode::from(4001),
        reason: "bye".into(),
    }))
    .await
    .unwrap();
    assert_eq!(
        event(&mut messages).await,
        Some(Event::Close {
            code: 4001,
            reason: "bye".to_owned(),
            clean: true
        })
    );
    assert_eq!(
        messages.until_end().await.unwrap(),
        b"",
        "the stream ends after the close"
    );
}

#[tokio::test]
async fn an_offer_is_taken_only_if_it_is_one_and_only_with_a_subprotocol_that_was_offered() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();

    // A request that offers nothing cannot be upgraded, and the page may answer it as it pleases.
    let plain = plain_get(served.url("/chat"));
    let seen = next_request(&mut served.requests).await.unwrap();
    assert!(!seen.upgrade);
    assert!(seen.protocols.is_empty());
    let refused_plain = take(&app, seen.id, None).await;
    assert_eq!(
        refused_plain.err().unwrap().code,
        ErrorCode::InvalidArgument
    );
    respond(&app, seen.id, 426, &[("sec-websocket-version", "13")], b"")
        .await
        .unwrap();
    assert_eq!(
        plain.await.unwrap().unwrap().0,
        StatusCode::UPGRADE_REQUIRED
    );

    // An offer with a subprotocol that was not offered is refused, and the page may still take it.
    let joining = dial(served.port, "/chat", &[], &["chat"]).await;
    let seen = next_request(&mut served.requests).await.unwrap();
    let other = take(&app, seen.id, Some("superchat")).await;
    assert_eq!(other.err().unwrap().code, ErrorCode::InvalidArgument);
    let (_, _) = take(&app, seen.id, Some("chat")).await.unwrap();
    let (_, handshake) = joining.await.unwrap().unwrap();
    assert_eq!(handshake.headers()["sec-websocket-protocol"], "chat");

    // An offer the page answers with a status is an answer like the others.
    let joining = dial(served.port, "/", &[], &[]).await;
    let seen = next_request(&mut served.requests).await.unwrap();
    respond(&app, seen.id, 403, &[], b"no").await.unwrap();
    assert_eq!(refused(joining.await.unwrap()), 403);
    let gone = take(&app, seen.id, None).await;
    assert_eq!(
        gone.err().unwrap().code,
        ErrorCode::NotFound,
        "a request is answered once"
    );

    // The id of a server is no request, and the server is left as it was.
    let not_a_request = app
        .call("http.upgrade", json!({ "request": served.id }))
        .await;
    assert_eq!(not_a_request.unwrap_err().code, ErrorCode::NotFound);
    app.call("socket.close", json!({ "socket": served.id }))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_handshake_that_is_not_one_is_a_request_like_the_others() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    for headers in [
        vec![("sec-websocket-version", "12")],
        vec![("sec-websocket-key", "short")],
        vec![("connection", "keep-alive")],
    ] {
        let mut sent = vec![
            ("connection", "Upgrade".to_owned()),
            ("upgrade", "websocket".to_owned()),
            ("sec-websocket-version", "13".to_owned()),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==".to_owned()),
        ];
        for (name, value) in &headers {
            sent.retain(|(have, _)| have != name);
            sent.push((*name, (*value).to_owned()));
        }
        let call = tokio::spawn(fetch("GET", served.url("/"), sent, Vec::new()));
        let seen = next_request(&mut served.requests)
            .await
            .expect("it comes to the page");
        assert!(!seen.upgrade, "{headers:?}");
        respond(&app, seen.id, 426, &[], b"").await.unwrap();
        assert_eq!(call.await.unwrap().unwrap().0, StatusCode::UPGRADE_REQUIRED);
    }
}

#[tokio::test]
async fn an_origin_that_is_not_the_servers_is_refused_before_the_page_hears_of_it() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({ "origins": ["https://app.test"] }))
        .await
        .unwrap();
    for origin in ["http://evil.test", "null", "https://app.test:8443"] {
        let joined = dial(served.port, "/", &[("origin", origin.to_owned())], &[]).await;
        assert_eq!(refused(joined.await.unwrap()), 403, "{origin}");
        assert!(
            nothing_comes(&mut served.requests).await,
            "{origin} reached the page"
        );
    }
    let joining = dial(
        served.port,
        "/",
        &[("origin", "https://app.test".to_owned())],
        &[],
    )
    .await;
    let seen = next_request(&mut served.requests)
        .await
        .expect("an origin the page lists");
    assert_eq!(seen.one("origin"), "https://app.test");
    take(&app, seen.id, None).await.unwrap();
    joining.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_message_over_the_limit_ends_the_connection_whether_it_comes_whole_or_in_fragments() {
    for fragments in [1_usize, 17] {
        let app = app_for(&[]).await;
        let mut served = serve(&app, json!({})).await.unwrap();
        let joining = dial(served.port, "/", &[], &[]).await;
        let seen = next_request(&mut served.requests).await.unwrap();
        let (_, mut messages) = take(&app, seen.id, None).await.unwrap();
        let (mut peer, _) = joining.await.unwrap().unwrap();
        let total = 17 * 1024 * 1024;
        let sent = if fragments == 1 {
            peer.send(Message::binary(vec![7_u8; total])).await
        } else {
            let piece = total / fragments;
            let mut outcome = Ok(());
            for index in 0..fragments {
                let opcode = if index == 0 {
                    OpData::Binary
                } else {
                    OpData::Continue
                };
                let frame = WsFrame::message(
                    vec![7_u8; piece],
                    OpCode::Data(opcode),
                    index + 1 == fragments,
                );
                outcome = peer.send(Message::Frame(frame)).await;
                if outcome.is_err() {
                    break;
                }
            }
            outcome
        };
        // The peer may be cut off while it still writes: the page's stream tells the rest.
        let _ = sent;
        match messages.frame().await {
            Some(Frame::Error(error)) => assert_eq!(error.code, ErrorCode::Network, "{fragments}"),
            other => {
                panic!("a message over the limit was taken ({fragments} fragments): {other:?}")
            }
        }
    }
}

#[tokio::test]
async fn an_offer_over_tls_is_taken_as_well() {
    let app = app_for(&[]).await;
    let mut served = serve(
        &app,
        json!({ "tls": { "cert": tls::certificate(), "key": tls::key() } }),
    )
    .await
    .unwrap();
    let port = served.port;
    let joining = tokio::spawn(async move {
        let secure = secure_connect(port).await.unwrap();
        let uri = format!("wss://127.0.0.1:{port}/secure").parse().unwrap();
        client_async(ClientRequestBuilder::new(uri), secure).await
    });
    let seen = next_request(&mut served.requests).await.unwrap();
    assert!(seen.upgrade);
    let (socket, mut messages) = take(&app, seen.id, None).await.unwrap();
    let (mut peer, handshake) = joining.await.unwrap().unwrap();
    assert_eq!(handshake.status(), 101);
    peer.send(Message::text("over TLS")).await.unwrap();
    assert_eq!(
        event(&mut messages).await,
        Some(Event::Text("over TLS".to_owned()))
    );
    send(&app, socket, true, b"and back").await;
    let Message::Text(text) = heard_secure(&mut peer).await else {
        panic!("a text message came back");
    };
    assert_eq!(text.as_str(), "and back");
}

/// The next message of a client over TLS.
async fn heard_secure<S>(peer: &mut WebSocketStream<TokioAdapter<S>>) -> Message
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(20), peer.next())
        .await
        .expect("the page sent nothing")
        .expect("the connection ended")
        .expect("the connection broke")
}

#[tokio::test]
async fn a_connection_outlives_the_server_that_took_it_and_goes_with_the_document() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    let joining = dial(served.port, "/", &[], &[]).await;
    let seen = next_request(&mut served.requests).await.unwrap();
    let (socket, mut messages) = take(&app, seen.id, None).await.unwrap();
    let (mut peer, _) = joining.await.unwrap().unwrap();
    app.call("socket.close", json!({ "socket": served.id }))
        .await
        .unwrap();
    peer.send(Message::text("still here")).await.unwrap();
    assert_eq!(
        event(&mut messages).await,
        Some(Event::Text("still here".to_owned()))
    );
    send(&app, socket, true, b"and here").await;
    assert_eq!(heard(&mut peer).await, Message::text("and here"));
    // The document goes: the resource of the connection is closed, and the connection ends for the client.
    app.session().close().await;
    assert!(ends(peer).await, "a connection outlived the document");
}

#[tokio::test]
async fn a_client_that_left_before_the_page_took_the_offer_is_told_to_the_page() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    let joining = dial(served.port, "/", &[], &[]).await;
    let seen = next_request(&mut served.requests).await.unwrap();
    joining.abort();
    // The client's end is gone with the aborted task. The page is told so when it takes the offer, or it
    // is handed a connection that is over, which ends its stream: as a drop, or with the error of the network.
    if let Ok((_, mut messages)) = take(&app, seen.id, None).await {
        match messages.frame().await {
            Some(Frame::Json(close)) => assert_eq!(close["type"], "close"),
            Some(Frame::Error(error)) => assert_eq!(error.code, ErrorCode::Network),
            other => panic!("{other:?}"),
        }
    }
}
