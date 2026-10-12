// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use alef_core::{
    protocol::{call::Limits, frame::Frame},
    security::permissions::{PathVars, PermissionSet},
    session::{session::SessionManager, streams::StreamReader},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
const WAIT: Duration = Duration::from_secs(5);

async fn session() -> Arc<Session> {
    SessionManager::new(
        Arc::new(|| "test-token".to_owned()),
        Limits {
            max_resources_per_session: 1,
            ..Limits::default()
        },
    )
    .begin_document(1)
    .await
}
fn permissions() -> Arc<PermissionSet> {
    let path = std::env::temp_dir();
    let vars = PathVars {
        app_data: path.clone(),
        app_config: path.clone(),
        app_cache: path.clone(),
        home: path.clone(),
        documents: path.clone(),
        downloads: path.clone(),
        desktop: path.clone(),
        temp: path.clone(),
        app: Some(path),
    };
    let mut policy = alef_core::security::manifest::Manifest::from_ktav_str(include_str!(
        "../../../../tests/fixtures/app.ktav"
    ))
    .unwrap()
    .permissions;
    policy.net.socket = vec!["listen:127.0.0.1:*".to_owned()];
    Arc::new(PermissionSet::from_manifest(&policy, &vars).unwrap())
}
struct Server {
    shared: Arc<Shared>,
    reader: StreamReader,
    address: SocketAddr,
    socket: Box<Socket>,
}
impl Server {
    async fn new(requests: usize, connections: usize, answer_time: Duration) -> Self {
        let session = session().await;
        let listener = timeout(WAIT, TcpListener::bind("127.0.0.1:0"))
            .await
            .unwrap()
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (writer, id) = session.streams().open_outgoing();
        let reader = session.streams().reader(id).unwrap();
        let shared = Arc::new(Shared {
            exchanges: exchanges::Table::new(&session, requests),
            files: Files::new(None, permissions(), session.grants()),
            session,
            requests: writer,
            guard: Guard::new(address, &[], &[]),
            answer_time,
            max_connections: connections,
        });
        let socket = Box::<Socket>::default();
        socket.adopt([tokio::spawn(accept_loop(listener, None, shared.clone()))]);
        Self {
            shared,
            reader,
            address,
            socket,
        }
    }
    async fn client(&self) -> TcpStream {
        let mut client = timeout(WAIT, TcpStream::connect(self.address))
            .await
            .unwrap()
            .unwrap();
        timeout(
            WAIT,
            client
                .write_all(format!("GET / HTTP/1.1\r\nHost: {}\r\n\r\n", self.address).as_bytes()),
        )
        .await
        .unwrap()
        .unwrap();
        client
    }
    async fn next(&mut self) -> u64 {
        match timeout(WAIT, self.reader.next_frame())
            .await
            .unwrap()
            .unwrap()
        {
            Frame::Json(frame) => frame["id"].as_u64().unwrap(),
            other => panic!("{other:?}"),
        }
    }
    async fn close(self) {
        use alef_core::session::resources::Resource;
        timeout(WAIT, self.socket.close()).await.unwrap();
    }
}
async fn reply(client: &mut TcpStream) -> String {
    let mut bytes = vec![0; 4096];
    let n = timeout(WAIT, client.read(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8_lossy(&bytes[..n]).into_owned()
}
fn respond(session: &Arc<Session>, id: u64) {
    let exchange = exchanges::Table::take(session, id, |_| Ok(())).unwrap();
    let _ = exchange.answer.send(Answer::Response {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Empty::new().map_err(|never| match never {}).boxed(),
    });
}
async fn gone(session: &Arc<Session>, id: u64) {
    // Observe without consuming: removing an entry here would mask a failed cleanup.
    timeout(WAIT, async {
        loop {
            let result = exchanges::Table::take(session, id, |_| Err(invalid("still live")));
            if result.err().unwrap().code == ErrorCode::NotFound && !exchanges::Table::routed(id) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// A server opened by the registered handler, with no test-built Shared or listener.
struct RegisteredServer {
    registry: Registry,
    context: CallContext,
    reader: StreamReader,
    address: SocketAddr,
}
impl RegisteredServer {
    async fn serve(args: serde_json::Value) -> Self {
        use alef_core::{
            ids::StreamId,
            security::consent::{Consent, Right},
        };
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut consent = Consent::undecided();
        consent.set(
            Right::scoped("net.socket", "listen:127.0.0.1:*"),
            Decision::Allow,
        );
        let permissions = Arc::new((*permissions()).clone().with_consent(consent));
        let context = CallContext::new(timeout(WAIT, session()).await.unwrap(), permissions);
        let Reply::Json(reply) =
            timeout(WAIT, registry.dispatch("http.serve", context.clone(), args))
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected a JSON server reply");
        };
        let address = SocketAddr::new(
            reply["address"]["host"].as_str().unwrap().parse().unwrap(),
            reply["address"]["port"].as_u64().unwrap() as u16,
        );
        assert_ne!(address.port(), 0, "the handler must bind a real listener");
        let reader = context
            .streams()
            .reader(StreamId(reply["requests"].as_u64().unwrap()))
            .unwrap();
        Self {
            registry,
            context,
            reader,
            address,
        }
    }
    async fn connect(&self) -> TcpStream {
        timeout(WAIT, TcpStream::connect(self.address))
            .await
            .unwrap()
            .unwrap()
    }
    async fn request(&self) -> TcpStream {
        let mut client = self.connect().await;
        timeout(
            WAIT,
            client
                .write_all(format!("GET / HTTP/1.1\r\nHost: {}\r\n\r\n", self.address).as_bytes()),
        )
        .await
        .unwrap()
        .unwrap();
        client
    }
    async fn next(&mut self) -> u64 {
        let Frame::Json(frame) = timeout(WAIT, self.reader.next_frame())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected a request frame");
        };
        frame["id"].as_u64().unwrap()
    }
    async fn respond(&self, id: u64) {
        timeout(
            WAIT,
            self.registry
                .dispatch("http.respond", self.context.clone(), json!({"request": id})),
        )
        .await
        .unwrap()
        .unwrap();
    }
    async fn close(&self) {
        // Session close awaits the real Socket resource and its accept/connection tasks.
        timeout(WAIT, self.context.session.close()).await.unwrap();
    }
}
async fn assert_closed(client: &mut TcpStream) {
    let mut byte = [0];
    let result = timeout(WAIT, client.read(&mut byte)).await.unwrap();
    assert!(result.is_err() || result.unwrap() == 0, "expected closure");
}

#[tokio::test]
async fn registered_serve_enforces_explicit_and_default_request_caps() {
    for (args, cap) in [
        (json!({"maxRequests": 3, "maxConnections": 5}), 3),
        (json!({}), DEFAULT_MAX_REQUESTS),
    ] {
        let mut server = RegisteredServer::serve(args).await;
        let mut clients = Vec::new();
        let mut ids = Vec::new();
        // Every slot must actually reach the page, including the last allowed slot.
        for _ in 0..cap {
            clients.push(server.request().await);
            ids.push(server.next().await);
        }
        let mut excess = server.request().await;
        assert!(reply(&mut excess).await.starts_with("HTTP/1.1 503"));
        server.close().await;
        for id in ids {
            gone(&server.context.session, id).await;
        }
        for client in &mut clients {
            assert_closed(client).await;
        }
    }
}

#[tokio::test]
async fn registered_serve_enforces_explicit_and_default_connection_caps() {
    for (args, cap) in [
        (json!({"maxConnections": 3, "maxRequests": 2}), 3),
        (json!({}), DEFAULT_MAX_CONNECTIONS),
    ] {
        let mut server = RegisteredServer::serve(args).await;
        let mut clients = Vec::new();
        for _ in 0..cap {
            let mut client = server.request().await;
            let id = server.next().await;
            server.respond(id).await;
            assert!(reply(&mut client).await.starts_with("HTTP/1.1 200"));
            gone(&server.context.session, id).await;
            // Keep the connection alive, but release its request slot so the default
            // request budget cannot mask the larger default connection budget.
            clients.push(client);
        }
        let mut excess = server.connect().await;
        assert_closed(&mut excess).await;
        server.close().await;
        for client in &mut clients {
            assert_closed(client).await;
        }
    }
}

#[tokio::test]
async fn server_limits_reject_invalid_values_and_accept_bounded_positive_integers() {
    let mut registry = Registry::default();
    register(&mut registry).unwrap();
    use alef_core::security::consent::{Consent, Right};
    let mut consent = Consent::undecided();
    consent.set(
        Right::scoped("net.socket", "listen:127.0.0.1:*"),
        Decision::Substitute,
    );
    let permissions = Arc::new((*permissions()).clone().with_consent(consent));
    let context = CallContext::new(session().await, permissions);
    for key in ["maxConnections", "maxRequests"] {
        for value in [
            json!(0),
            json!(-1),
            json!(257),
            json!(1.5),
            json!("2"),
            json!(true),
            json!(null),
        ] {
            let parsed = serde_json::from_value::<ServeArgs>(json!({key: value.clone()}));
            assert!(
                parsed.is_err(),
                "{key} must reject non-positive/non-integral values"
            );
            let result = timeout(
                WAIT,
                registry.dispatch(
                    "http.serve",
                    context.clone().with_decision(Decision::Substitute),
                    json!({key: value}),
                ),
            )
            .await
            .unwrap();
            assert_eq!(result.unwrap_err().code, ErrorCode::InvalidArgument);
        }
        for value in [1, 256] {
            let args: ServeArgs = serde_json::from_value(json!({key: value})).unwrap();
            assert_eq!(
                budget(args.max_connections.or(args.max_requests), 64).unwrap(),
                value as usize
            );
        }
    }
    let substituted = timeout(
        WAIT,
        registry.dispatch(
            "http.serve",
            context.clone(),
            json!({"maxConnections": 1, "maxRequests": 1}),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(substituted, Reply::Json(_)));
    timeout(WAIT, context.session.close()).await.unwrap();
    assert_eq!(budget(None, 64).unwrap(), 64);
    assert_eq!(budget(None, 32).unwrap(), 32);
}

#[tokio::test]
async fn exchanges_enforce_ownership_and_keep_slots_until_their_leases_are_dropped() {
    let owner = session().await;
    let other = session().await; // Separate managers may reuse SessionId; ownership still differs.
    let table = exchanges::Table::new(&owner, 1);
    let make = || {
        let (answer, _) = oneshot::channel();
        Exchange {
            answer,
            offer: None,
        }
    };
    let lease = table.insert(make()).unwrap();
    assert!(table.insert(make()).is_none());
    assert_eq!(
        exchanges::Table::take(&other, lease.id, |_| Ok(()))
            .err()
            .unwrap()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        exchanges::Table::take(&owner, lease.id, |_| Err(invalid("reject")))
            .err()
            .unwrap()
            .code,
        ErrorCode::InvalidArgument
    );
    let id = lease.id;
    let _ = exchanges::Table::take(&owner, id, |_| Ok(())).unwrap();
    assert!(
        !exchanges::Table::routed(id),
        "take must remove routing immediately"
    );
    assert!(
        table.insert(make()).is_none(),
        "taking a sender must not release the handler slot"
    );
    drop(lease);
    let next = table.insert(make()).unwrap();
    assert!(next.id > id && next.id < (1 << 53));
    drop(next);
    gone(&owner, id).await;
}

#[tokio::test]
async fn dropping_a_lease_or_closing_a_server_removes_routing_synchronously() {
    let owner = session().await;
    let table = exchanges::Table::new(&owner, 2);
    let make = || {
        let (answer, _) = oneshot::channel();
        Exchange {
            answer,
            offer: None,
        }
    };
    let lease = table.insert(make()).unwrap();
    let id = lease.id;
    drop(lease);
    assert!(!exchanges::Table::routed(id), "lease must remove route");
    assert!(!table.contains(id), "lease must remove entry");
    let lease = table.insert(make()).unwrap();
    let id = lease.id;
    drop(exchanges::Closing(table.clone()));
    assert!(
        !exchanges::Table::routed(id),
        "closing must remove route before lease drops"
    );
    assert!(
        !table.contains(id),
        "closing must remove entry before lease drops"
    );
    assert!(
        table.insert(make()).is_none(),
        "closed table must reject insert"
    );
    drop(lease);
}

#[tokio::test]
async fn pending_exchanges_allow_unrelated_resources_and_excess_requests_receive_503() {
    let mut server = Server::new(1, 3, WAIT).await;
    let mut first = server.client().await;
    let id = server.next().await;
    // Inserting an unrelated resource must succeed while the exchange is still held.
    server
        .shared
        .session
        .resources()
        .insert(Box::<Socket>::default())
        .unwrap();
    let mut excess = server.client().await;
    assert!(reply(&mut excess).await.starts_with("HTTP/1.1 503"));
    respond(&server.shared.session, id);
    assert!(reply(&mut first).await.starts_with("HTTP/1.1 200"));
    gone(&server.shared.session, id).await;
    let mut next = server.client().await;
    let id = server.next().await;
    respond(&server.shared.session, id);
    assert!(reply(&mut next).await.starts_with("HTTP/1.1 200"));
    server.close().await;
}

#[tokio::test]
async fn the_connection_cap_closes_excess_clients_and_recovers_after_tasks_complete() {
    let mut server = Server::new(2, 1, WAIT).await;
    let first = server.client().await;
    let id = server.next().await;
    let mut excess = timeout(WAIT, TcpStream::connect(server.address))
        .await
        .unwrap()
        .unwrap();
    let mut byte = [0];
    let closed = timeout(WAIT, excess.read(&mut byte)).await.unwrap();
    assert!(closed.is_err() || closed.unwrap() == 0);
    drop(first);
    gone(&server.shared.session, id).await;
    // Route removal can precede task completion. Retry rejected connections until
    // admission recovers, under the same deadline rather than extending the timeout.
    let (mut next, id) = timeout(WAIT, async {
        loop {
            let mut client = TcpStream::connect(server.address).await.unwrap();
            if client
                .write_all(format!("GET / HTTP/1.1\r\nHost: {}\r\n\r\n", server.address).as_bytes())
                .await
                .is_err()
            {
                continue;
            }
            tokio::select! {
                frame = server.reader.next_frame() => {
                    let Frame::Json(frame) = frame.unwrap() else {
                        panic!("expected a request frame");
                    };
                    break (client, frame["id"].as_u64().unwrap());
                }
                closed = client.read(&mut byte) => {
                    assert!(closed.is_err() || closed.unwrap() == 0);
                }
            }
        }
    })
    .await
    .unwrap();
    respond(&server.shared.session, id);
    assert!(reply(&mut next).await.starts_with("HTTP/1.1 200"));
    server.close().await;
}

#[tokio::test]
async fn dropped_and_half_closed_clients_cancel_their_pending_handlers() {
    let mut server = Server::new(1, 3, Duration::from_secs(60)).await;
    let client = server.client().await;
    let id = server.next().await;
    drop(client);
    gone(&server.shared.session, id).await;
    let mut half = server.client().await;
    let id = server.next().await;
    timeout(WAIT, half.shutdown()).await.unwrap().unwrap();
    gone(&server.shared.session, id).await;
    assert_eq!(reply(&mut half).await, "");
    server.close().await;
}

#[tokio::test]
async fn losing_an_answer_sender_from_the_exchange_table_returns_500_and_releases_the_slot() {
    let mut server = Server::new(1, 2, Duration::from_secs(60)).await;
    let mut client = server.client().await;
    let id = server.next().await;
    assert!(server.shared.exchanges.contains(id));
    // Lose the real sender without closing the server or cancelling its handler.
    drop(exchanges::Table::take(&server.shared.session, id, |_| Ok(())).unwrap());
    assert!(reply(&mut client).await.starts_with("HTTP/1.1 500"));
    gone(&server.shared.session, id).await;
    assert!(!server.shared.exchanges.contains(id));
    let mut next = server.client().await;
    let next_id = server.next().await;
    respond(&server.shared.session, next_id);
    assert!(reply(&mut next).await.starts_with("HTTP/1.1 200"));
    server.close().await;
}

#[tokio::test]
async fn timeouts_send_failures_and_server_closure_remove_request_routes() {
    let mut server = Server::new(1, 2, Duration::from_millis(20)).await;
    let mut client = server.client().await;
    let id = server.next().await;
    assert!(reply(&mut client).await.starts_with("HTTP/1.1 504"));
    gone(&server.shared.session, id).await;
    server
        .shared
        .session
        .streams()
        .close(server.reader.id())
        .unwrap();
    let mut failed = server.client().await;
    assert!(reply(&mut failed).await.starts_with("HTTP/1.1 503"));
    server.close().await;
    let mut server = Server::new(1, 2, WAIT).await;
    let _client = server.client().await;
    let id = server.next().await;
    let owner = server.shared.session.clone();
    server.close().await;
    gone(&owner, id).await;
}

#[tokio::test]
async fn closing_a_session_removes_its_pending_exchanges() {
    let mut server = Server::new(1, 2, WAIT).await;
    // Use the real Socket resource close/await path owned by the session.
    let socket = std::mem::take(&mut server.socket);
    server.shared.session.resources().insert(socket).unwrap();
    let _client = server.client().await;
    let id = server.next().await;
    timeout(WAIT, server.shared.session.close()).await.unwrap();
    gone(&server.shared.session, id).await;
    server.close().await;
}
