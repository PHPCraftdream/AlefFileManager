// SPDX-License-Identifier: MIT OR Apache-2.0
//! `http.serve`: a server of HTTP on a port of this machine whose requests go to the page. The port is
//! taken as `listen:host:port` of `permissions.net.socket` allows (the loopback when the page names no
//! host), the `Host` and the `Origin` of a request are held against the names and the origins of the
//! server, and what comes goes to the page as frames of a stream with credit, one frame for each request,
//! with the body of the request as a stream of its own. The page answers with `http.respond` (a body that
//! is small) or `http.respondStream` (a stream). A folder (`files`) is served from the disk without a
//! word to the page. The right the user substituted gives a port that nobody comes to.
use std::{
    any::Any, convert::Infallible, future::Future, net::SocketAddr, pin::Pin, sync::Arc,
    time::Duration,
};

use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::{
        consent::Decision,
        permissions::{Permission, Reach},
    },
    session::{resources::Resource, session::Session, streams::StreamWriter},
    AlefError, ErrorCode,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{
    body::{Body, Incoming},
    server::conn::http1,
    service::service_fn,
    HeaderMap, Request, Response, StatusCode,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use rustls::{
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    ServerConfig,
};
use serde::Deserialize;
use serde_json::json;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinSet,
};
use tokio_rustls::TlsAcceptor;

use super::{
    body::{from_stream, pump, RequestBody},
    files::{plain, Files},
    guard::Guard,
};
use crate::{
    json,
    net::{
        headers,
        socket::{standing, target, Socket, LOOPBACK},
    },
};

/// How long the page has to take a request and to answer it, before the client gets a 503 or a 504,
/// when the page names no time.
const ANSWER_TIME: Duration = Duration::from_secs(60);
/// How long a client has to send the head of a request.
const HEAD_TIME: Duration = Duration::from_secs(10);
/// How long an accept that fails waits before it tries again.
const ACCEPT_PAUSE: Duration = Duration::from_millis(50);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TlsPair {
    cert: String,
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ServeArgs {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    tls: Option<TlsPair>,
    #[serde(default)]
    files: Option<String>,
    #[serde(default)]
    hosts: Vec<String>,
    #[serde(default)]
    origins: Vec<String>,
    #[serde(default)]
    answer_timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RespondArgs {
    request: u64,
    #[serde(default = "ok")]
    status: u16,
    #[serde(default)]
    headers: Vec<(String, String)>,
}

fn ok() -> u16 {
    200
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// What the page answers a request with.
struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: RequestBody,
}

/// A request the page has not answered yet: where its answer goes.
struct Exchange {
    answer: oneshot::Sender<Answer>,
}

impl Resource for Exchange {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}

/// Removes the resource of a request when the request is over, whichever way it ended.
struct Forget {
    session: Arc<Session>,
    id: ResourceId,
}

impl Drop for Forget {
    fn drop(&mut self) {
        let _ = self.session.resources().take(self.id);
    }
}

/// What every connection of a server shares.
struct Shared {
    session: Arc<Session>,
    requests: StreamWriter,
    guard: Guard,
    files: Files,
    answer_time: Duration,
}

fn text(status: StatusCode, message: &'static str) -> Response<RequestBody> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(message.as_bytes()))
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    response.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// The request as the page sees it, once its head is let in and the page has room for it.
async fn answer_request(shared: &Shared, request: Request<Incoming>) -> Response<RequestBody> {
    if let Some((status, message)) = shared.guard.check(request.headers()) {
        return text(status, message);
    }
    if let Some(file) = shared
        .files
        .answer(request.method(), request.uri().path())
        .await
    {
        return file;
    }
    let (parts, incoming) = request.into_parts();
    let (sender, receiver) = oneshot::channel();
    let Ok(id) = shared
        .session
        .resources()
        .insert(Box::new(Exchange { answer: sender }))
    else {
        return text(
            StatusCode::SERVICE_UNAVAILABLE,
            "the application has no room for another request",
        );
    };
    let _forget = Forget {
        session: shared.session.clone(),
        id,
    };
    let body = if incoming.is_end_stream() {
        None
    } else {
        let (writer, stream) = shared.session.streams().open_outgoing();
        pump(incoming, writer);
        Some(stream.0)
    };
    let headers: Vec<[String; 2]> = parts
        .headers
        .iter()
        .map(|(name, value)| {
            [
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            ]
        })
        .collect();
    let url = parts
        .uri
        .path_and_query()
        .map_or("/", |target| target.as_str());
    let frame = json!({
        "id": id.0,
        "method": parts.method.as_str(),
        "url": url,
        "headers": headers,
        "body": body,
    });
    match tokio::time::timeout(shared.answer_time, shared.requests.send_json(frame)).await {
        Ok(Ok(())) => {}
        _ => {
            return text(
                StatusCode::SERVICE_UNAVAILABLE,
                "the application takes no requests",
            )
        }
    }
    match tokio::time::timeout(shared.answer_time, receiver).await {
        Ok(Ok(answer)) => {
            let mut response = Response::new(answer.body);
            *response.status_mut() = answer.status;
            *response.headers_mut() = answer.headers;
            response
        }
        Ok(Err(_)) => text(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the application dropped the request",
        ),
        Err(_) => text(
            StatusCode::GATEWAY_TIMEOUT,
            "the application did not answer in time",
        ),
    }
}

async fn serve_io<I>(io: I, shared: Arc<Shared>)
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let service = service_fn(move |request| {
        let shared = shared.clone();
        async move { Ok::<_, Infallible>(answer_request(&shared, request).await) }
    });
    let _ = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEAD_TIME)
        .serve_connection(io, service)
        .await;
}

async fn connection(stream: TcpStream, acceptor: Option<TlsAcceptor>, shared: Arc<Shared>) {
    match acceptor {
        None => serve_io(TokioIo::new(stream), shared).await,
        Some(acceptor) => {
            if let Ok(secure) = acceptor.accept(stream).await {
                serve_io(TokioIo::new(secure), shared).await;
            }
        }
    }
}

/// Takes the connections of the listener; the connections end with the task, for they are its own.
async fn accept_loop(listener: TcpListener, acceptor: Option<TlsAcceptor>, shared: Arc<Shared>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let _ = stream.set_nodelay(true);
                    connections.spawn(connection(stream, acceptor.clone(), shared.clone()));
                }
                Err(_) => tokio::time::sleep(ACCEPT_PAUSE).await,
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

fn tls_acceptor(pair: &TlsPair) -> Result<TlsAcceptor, AlefError> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pair.cert.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|_| invalid("cert is not PEM certificates"))?;
    let key = PrivateKeyDer::from_pem_slice(pair.key.as_bytes())
        .map_err(|_| invalid("key is not a PEM private key"))?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|error| invalid(&format!("cert and key do not go together: {error}")))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// The folder of `files` as the application may read it: `None` for a folder a stand-in replaced.
fn files_root(ctx: &CallContext, folder: &str) -> Result<Option<std::path::PathBuf>, AlefError> {
    let allowed = ctx.permissions.authorize_at(
        Permission::FsRead,
        Some(folder),
        &ctx.grants(),
        Reach::Through,
    )?;
    if allowed.shadow.is_some() {
        return Ok(None);
    }
    let real = allowed
        .path
        .canonicalize()
        .map_err(|_| invalid("files is a folder that exists"))?;
    if !real.is_dir() {
        return Err(invalid("files is a folder"));
    }
    Ok(Some(plain(real)))
}

/// The place where the answer of a request goes, taken for good: a request is answered once.
fn take_answer(ctx: &CallContext, request: u64) -> Result<oneshot::Sender<Answer>, AlefError> {
    let id = ResourceId(request);
    // Only a request is taken: the id of a server is no request, and is left as it is.
    ctx.resources().with_as::<Exchange, _>(id, |_| ())?;
    let any: Box<dyn Any> = ctx.resources().take(id)?;
    any.downcast::<Exchange>()
        .map(|exchange| exchange.answer)
        .map_err(|_| AlefError::new(ErrorCode::NotFound, "resource not found"))
}

fn status_of(code: u16) -> Result<StatusCode, AlefError> {
    StatusCode::from_u16(code)
        .ok()
        .filter(|status| (200..=599).contains(&status.as_u16()))
        .ok_or_else(|| invalid("a status is 200 to 599"))
}

pub(super) fn register(registry: &mut Registry) -> Result<(), AlefError> {
    registry
        .command::<ServeArgs>("http.serve")?
        .permission(Permission::NetSocket, |args| {
            Some(target(
                "listen",
                args.host.as_deref().unwrap_or(LOOPBACK),
                args.port,
            ))
        })
        .substitutes()
        .handler(|ctx, args| async move {
            let host = args.host.as_deref().unwrap_or(LOOPBACK);
            let (writer, requests) = ctx.streams().open_outgoing();
            if ctx.decision() == Decision::Substitute {
                // A port nobody took: the page gets an address, and nobody comes to it.
                let id = ctx.resources().insert(Box::new(standing(writer)))?;
                let address = json!({ "host": host, "port": args.port });
                return json(&json!({ "server": id.0, "requests": requests.0, "address": address, "secure": args.tls.is_some() }));
            }
            let root = match args.files.as_deref() {
                Some(folder) => files_root(&ctx, folder)?,
                None => None,
            };
            let acceptor = args.tls.as_ref().map(tls_acceptor).transpose()?;
            let listener = TcpListener::bind((host, args.port)).await?;
            let local: SocketAddr = listener.local_addr()?;
            let shared = Arc::new(Shared {
                session: ctx.session.clone(),
                requests: writer,
                guard: Guard::new(local, &args.hosts, &args.origins),
                files: Files::new(root, ctx.permissions.clone(), ctx.grants()),
                answer_time: args
                    .answer_timeout_ms
                    .map_or(ANSWER_TIME, Duration::from_millis),
            });
            let secure = acceptor.is_some();
            let id = ctx.resources().insert(Box::<Socket>::default())?;
            let task = tokio::spawn(accept_loop(listener, acceptor, shared));
            ctx.resources()
                .with_as::<Socket, _>(id, |socket| socket.adopt([task]))?;
            json(&json!({
                "server": id.0,
                "requests": requests.0,
                "address": { "host": local.ip().to_string(), "port": local.port() },
                "secure": secure,
            }))
        })?;

    registry
        .command::<RespondArgs>("http.respond")?
        .handler(|ctx, args| async move {
            let status = status_of(args.status)?;
            let headers = headers::parse(&args.headers, &[])?;
            let sender = take_answer(&ctx, args.request)?;
            let body: RequestBody = match ctx.body() {
                Some(bytes) => Full::new(bytes.clone())
                    .map_err(|never| match never {})
                    .boxed(),
                None => Empty::new().map_err(|never| match never {}).boxed(),
            };
            // A client that went away is no failure of the page.
            let _ = sender.send(Answer {
                status,
                headers,
                body,
            });
            Ok(Reply::Json(serde_json::Value::Null))
        })?;

    registry
        .command::<RespondArgs>("http.respondStream")?
        .handler(|ctx, args| async move {
            let status = status_of(args.status)?;
            let headers = headers::parse(&args.headers, &[])?;
            let sender = take_answer(&ctx, args.request)?;
            let (reader, upload) = ctx.streams().open_incoming_reader();
            let _ = sender.send(Answer {
                status,
                headers,
                body: from_stream(reader),
            });
            json(&json!({ "upload": upload.0 }))
        })
}
