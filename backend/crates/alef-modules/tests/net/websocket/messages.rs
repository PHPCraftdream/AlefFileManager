// SPDX-License-Identifier: MIT OR Apache-2.0
//! The messages each way, and the ends of a connection: the close of the page, the close of the server,
//! a connection that is dropped.
use super::*;
use crate::shared::manifest::app;

#[tokio::test]
async fn messages_go_each_way_as_text_or_bytes_and_empty_ones_too() {
    let server = WsServer::start().await;
    let app = app(&[server.scope()], &[]).await;
    let mut open = connect(&app, &server.url("/echo"), json!({}))
        .await
        .unwrap();
    assert_eq!(open.protocol, "", "no subprotocol was asked for");
    let text = "héllo — мир 🌍";
    for (as_text, data) in [
        (true, text.as_bytes().to_vec()),
        (false, vec![0, 255, 7]),
        (true, Vec::new()),
        (false, Vec::new()),
    ] {
        send(&app, open.id, as_text, &data).await.unwrap();
        let back = event(&mut open.messages).await.expect("an echo");
        let expected = if as_text {
            Event::Text(String::from_utf8(data).unwrap())
        } else {
            Event::Binary(data)
        };
        assert_eq!(back, expected);
    }
    assert_eq!(
        server.visits()[0].received,
        [
            Received::Text(text.to_owned()),
            Received::Binary(vec![0, 255, 7]),
            Received::Text(String::new()),
            Received::Binary(Vec::new()),
        ],
        "the server saw the kind of each message"
    );
    close(&app, open.id, None, "").await.unwrap();
}

#[tokio::test]
async fn a_big_message_comes_in_pieces_and_goes_back_whole() {
    let server = WsServer::start().await;
    let app = app(&[server.scope()], &[]).await;
    let mut open = connect(&app, &server.url("/big"), json!({})).await.unwrap();
    let Some(Event::Binary(first)) = event(&mut open.messages).await else {
        panic!("a binary message came first");
    };
    assert!(first == pattern(BIG), "the message came whole and in order");

    let data = pattern(250 * 1024);
    send(&app, open.id, false, &data).await.unwrap();
    assert_eq!(
        event(&mut open.messages).await,
        Some(Event::Binary(data)),
        "and one the page sends comes back"
    );
    close(&app, open.id, None, "").await.unwrap();
}

#[tokio::test]
async fn a_text_message_is_utf8_and_a_close_is_with_a_code_a_page_may_use() {
    let server = WsServer::start().await;
    let app = app(&[server.scope()], &[]).await;
    let open = connect(&app, &server.url("/echo"), json!({}))
        .await
        .unwrap();
    let broken = send(&app, open.id, true, &[0xff, 0xfe]).await;
    assert_eq!(code(broken), ErrorCode::InvalidArgument);
    assert!(server.visits()[0].received.is_empty(), "nothing was sent");
    for (given, reason) in [
        (Some(1005), ""),
        (Some(999), ""),
        (Some(1001), ""),
        (Some(5000), ""),
        (Some(4000), "x".repeat(124).as_str()),
    ] {
        let refused = close(&app, open.id, given, reason).await;
        assert_eq!(code(refused), ErrorCode::InvalidArgument, "{given:?}");
    }
    assert!(
        send(&app, open.id, true, b"still open").await.is_ok(),
        "a refusal leaves the connection as it was"
    );
    close(&app, open.id, Some(3000), &"x".repeat(123))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_page_closes_with_a_code_and_a_reason_and_the_peer_answers() {
    let server = WsServer::start().await;
    let app = app(&[server.scope()], &[]).await;
    let mut open = connect(&app, &server.url("/echo"), json!({}))
        .await
        .unwrap();
    close(&app, open.id, Some(4000), "done").await.unwrap();
    assert_eq!(
        event(&mut open.messages).await,
        Some(Event::Close {
            code: 4000,
            reason: "done".to_owned(),
            clean: true
        }),
        "the server answered with the same close"
    );
    assert_eq!(open.messages.until_end().await.unwrap(), b"");
    assert_eq!(
        server.visits()[0].close,
        Some((4000, "done".to_owned())),
        "the server saw the code and the reason"
    );
    let again = close(&app, open.id, None, "").await;
    assert_eq!(
        code(again),
        ErrorCode::NotFound,
        "a connection is closed once"
    );
    let late = send(&app, open.id, true, b"x").await;
    assert_eq!(code(late), ErrorCode::NotFound, "and then it is gone");

    let mut plain = connect(&app, &server.url("/echo"), json!({}))
        .await
        .unwrap();
    close(&app, plain.id, None, "").await.unwrap();
    assert_eq!(
        event(&mut plain.messages).await,
        Some(Event::Close {
            code: 1000,
            reason: String::new(),
            clean: true
        })
    );
}

#[tokio::test]
async fn the_server_closes_and_the_page_hears_why() {
    let server = WsServer::start().await;
    let app = app(&[server.scope()], &[]).await;
    let mut open = connect(&app, &server.url("/bye"), json!({})).await.unwrap();
    assert_eq!(
        event(&mut open.messages).await,
        Some(Event::Text("last".to_owned()))
    );
    assert_eq!(
        event(&mut open.messages).await,
        Some(Event::Close {
            code: 4001,
            reason: "bye".to_owned(),
            clean: true
        })
    );
    assert_eq!(open.messages.until_end().await.unwrap(), b"");
    close(&app, open.id, None, "").await.unwrap();
}

#[tokio::test]
async fn a_connection_that_is_dropped_ends_without_a_close() {
    let server = WsServer::start().await;
    let app = app(&[server.scope()], &[]).await;
    let mut open = connect(&app, &server.url("/abrupt"), json!({}))
        .await
        .unwrap();
    assert_eq!(
        event(&mut open.messages).await,
        Some(Event::Text("x".to_owned()))
    );
    assert_eq!(
        event(&mut open.messages).await,
        Some(Event::Close {
            code: 1006,
            reason: String::new(),
            clean: false
        })
    );
    assert_eq!(open.messages.until_end().await.unwrap(), b"");
    close(&app, open.id, None, "").await.unwrap();
}

#[tokio::test]
async fn a_close_without_a_code_is_1005_and_a_frame_against_the_protocol_fails_the_stream() {
    let server = WsServer::start().await;
    let app = app(&[server.scope()], &[]).await;
    let mut nocode = connect(&app, &server.url("/nocode"), json!({}))
        .await
        .unwrap();
    assert_eq!(
        event(&mut nocode.messages).await,
        Some(Event::Close {
            code: 1005,
            reason: String::new(),
            clean: true
        })
    );
    assert_eq!(nocode.messages.until_end().await.unwrap(), b"");

    let mut broken = connect(&app, &server.url("/broken"), json!({}))
        .await
        .unwrap();
    let ended = broken.messages.until_end().await.unwrap_err();
    assert_eq!(ended.code, ErrorCode::Network);
    assert!(
        ended.message.starts_with("the connection broke"),
        "{}",
        ended.message
    );
}
