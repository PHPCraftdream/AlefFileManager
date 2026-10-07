// SPDX-License-Identifier: MIT OR Apache-2.0
//! One instance of an application per user (`app.requestSingleInstance`). The first instance owns a
//! local endpoint: a named pipe on Windows, a Unix socket in a private folder of the application
//! cache elsewhere. A later instance finds it taken, hands over its command line and working
//! directory and learns it is not the first; the first one raises `app.second-instance` in its
//! documents. Only the user that owns the endpoint can reach it.
use std::{io, path::PathBuf, sync::Arc, time::Duration};

use alef_core::{registry::host::Host, AlefError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::Mutex,
};

use crate::{ModuleContext, ParsedArgs};

/// The event the first instance raises when another one starts.
pub(crate) const SECOND_INSTANCE: &str = "app.second-instance";

const MAX_MESSAGE: u64 = 256 * 1024;
const MAX_ARGUMENTS: usize = 4096;
const IO_LIMIT: Duration = Duration::from_secs(5);
const DELIVERY_ATTEMPTS: u32 = 5;
const RETRY_AFTER: Duration = Duration::from_millis(100);

/// What a later instance tells the first one.
#[derive(Debug, Serialize, Deserialize)]
struct Hello {
    args: ParsedArgs,
    cwd: String,
}

trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}
type Connection = Box<dyn Duplex>;

/// Whether this process got the endpoint or somebody else holds it.
enum Claim {
    First(platform::Listener),
    Taken,
}

/// Where the instances of one application of one user meet.
#[derive(Debug, Clone)]
pub(crate) struct Endpoint {
    /// The private folder that holds the socket (Unix; Windows names a pipe instead).
    #[cfg_attr(windows, allow(dead_code))]
    folder: PathBuf,
    /// Tells the application of the user from every other one.
    key: String,
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A Unix socket path has to be shorter than 104 bytes (macOS) or 108 (Linux, with the closing zero).
#[cfg_attr(windows, allow(dead_code))]
const SOCKET_PATH_LIMIT: usize = 100;

/// The first of `candidates` in which a socket `name` fits; the last one when none does (the
/// attempt to use it then says why it cannot be).
#[cfg_attr(windows, allow(dead_code))]
fn folder_that_fits(name: &str, candidates: Vec<PathBuf>) -> PathBuf {
    candidates
        .iter()
        .find(|folder| folder.join(name).as_os_str().len() < SOCKET_PATH_LIMIT)
        .or(candidates.last())
        .cloned()
        .unwrap_or_default()
}

impl Endpoint {
    pub(crate) fn of(context: &ModuleContext) -> Self {
        let identity = format!("{}|{}", context.paths.home.display(), context.app.id);
        let key = format!("{:016x}", fnv1a(&identity));
        Self {
            folder: Self::folder_for(&key, &context.paths.app_cache),
            key,
        }
    }

    /// Where the socket lives. The cache folder of an application with a long id or a deep home is
    /// often too long for a socket path, so the folders that are private to the user and short are
    /// tried first: the runtime folder of the session (Linux) and the temporary folder of the user
    /// (macOS).
    fn folder_for(key: &str, cache: &std::path::Path) -> PathBuf {
        let mut candidates = Vec::new();
        #[cfg(unix)]
        {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|folder| folder.is_absolute() && folder.is_dir());
            if let Some(runtime) = runtime {
                candidates.push(runtime.join("alef"));
            }
            #[cfg(target_os = "macos")]
            candidates.push(std::env::temp_dir().join("alef"));
        }
        candidates.push(cache.join("instance"));
        folder_that_fits(&format!("{key}.sock"), candidates)
    }
}

#[derive(Clone, Copy)]
enum State {
    Unasked,
    First,
    Later,
}

pub(crate) struct Instance {
    endpoint: Endpoint,
    args: ParsedArgs,
    host: Arc<dyn Host>,
    state: Mutex<State>,
}

fn unavailable(error: impl std::fmt::Display) -> AlefError {
    AlefError::new(ErrorCode::NotAvailable, format!("single instance: {error}"))
}

impl Instance {
    pub(crate) fn new(endpoint: Endpoint, args: ParsedArgs, host: Arc<dyn Host>) -> Self {
        Self {
            endpoint,
            args,
            host,
            state: Mutex::new(State::Unasked),
        }
    }

    /// `true` when this is the first instance; `false` when another one is running and has been
    /// told about this one. Asking again gives the same answer.
    pub(crate) async fn request(&self) -> Result<bool, AlefError> {
        let mut state = self.state.lock().await;
        match *state {
            State::First => return Ok(true),
            State::Later => return Ok(false),
            State::Unasked => {}
        }
        match platform::claim(&self.endpoint).await.map_err(unavailable)? {
            Claim::First(mut listener) => {
                let host = self.host.clone();
                tokio::spawn(async move {
                    loop {
                        match listener.accept().await {
                            Ok(connection) => {
                                let host = host.clone();
                                tokio::spawn(async move {
                                    let _ = tokio::time::timeout(IO_LIMIT, serve(connection, host))
                                        .await;
                                });
                            }
                            Err(_) => tokio::time::sleep(RETRY_AFTER).await,
                        }
                    }
                });
                *state = State::First;
                Ok(true)
            }
            Claim::Taken => {
                let cwd = std::env::current_dir().map_err(unavailable)?;
                let hello = Hello {
                    args: self.args.clone(),
                    cwd: cwd.to_string_lossy().into_owned(),
                };
                deliver(&self.endpoint, &hello).await.map_err(unavailable)?;
                *state = State::Later;
                Ok(false)
            }
        }
    }
}

/// Reads what a later instance says and raises the event; whoever sends anything else is ignored.
async fn serve(mut connection: Connection, host: Arc<dyn Host>) -> io::Result<()> {
    let mut line = Vec::new();
    BufReader::new((&mut connection).take(MAX_MESSAGE))
        .read_until(b'\n', &mut line)
        .await?;
    let hello: Hello = serde_json::from_slice(&line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if hello.args.raw.len() > MAX_ARGUMENTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "too many arguments",
        ));
    }
    host.emit(
        None,
        SECOND_INSTANCE,
        json!({ "args": hello.args, "cwd": hello.cwd }),
    );
    connection.write_all(b"ok\n").await?;
    connection.flush().await
}

async fn send(endpoint: &Endpoint, message: &[u8]) -> io::Result<()> {
    let mut connection = platform::connect(endpoint).await?;
    connection.write_all(message).await?;
    connection.flush().await?;
    let mut reply = [0u8; 3];
    connection.read_exact(&mut reply).await?;
    if &reply == b"ok\n" {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the first instance answered something else",
        ))
    }
}

/// Hands `hello` to the first instance; the endpoint may be busy for a moment.
async fn deliver(endpoint: &Endpoint, hello: &Hello) -> io::Result<()> {
    let mut message = serde_json::to_vec(hello)?;
    message.push(b'\n');
    let mut last = io::Error::from(io::ErrorKind::TimedOut);
    for attempt in 0..DELIVERY_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(RETRY_AFTER).await;
        }
        match tokio::time::timeout(IO_LIMIT, send(endpoint, &message)).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => last = error,
            Err(_) => last = io::Error::from(io::ErrorKind::TimedOut),
        }
    }
    Err(last)
}

#[cfg(unix)]
mod platform {
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        path::PathBuf,
    };

    use tokio::net::{UnixListener, UnixStream};

    use super::{io, Claim, Connection, Endpoint};

    pub(super) struct Listener {
        listener: UnixListener,
        path: PathBuf,
    }

    impl Listener {
        pub(super) async fn accept(&mut self) -> io::Result<Connection> {
            Ok(Box::new(self.listener.accept().await?.0))
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    pub(super) fn socket(endpoint: &Endpoint) -> PathBuf {
        endpoint.folder.join(format!("{}.sock", endpoint.key))
    }

    pub(super) async fn claim(endpoint: &Endpoint) -> io::Result<Claim> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&endpoint.folder)?;
        fs::set_permissions(&endpoint.folder, fs::Permissions::from_mode(0o700))?;
        let path = socket(endpoint);
        for _ in 0..2 {
            match UnixListener::bind(&path) {
                Ok(listener) => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
                    return Ok(Claim::First(Listener { listener, path }));
                }
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                    if UnixStream::connect(&path).await.is_ok() {
                        return Ok(Claim::Taken);
                    }
                    // Nobody listens: the socket is left over from a process that died.
                    fs::remove_file(&path)?;
                }
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                    return Err(io::Error::other(format!(
                        "the socket path is {} bytes long and a Unix socket takes fewer than 104",
                        path.as_os_str().len()
                    )));
                }
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("the instance socket could not be claimed"))
    }

    pub(super) async fn connect(endpoint: &Endpoint) -> io::Result<Connection> {
        Ok(Box::new(UnixStream::connect(socket(endpoint)).await?))
    }
}

#[cfg(windows)]
mod platform {
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

    use super::{io, Claim, Connection, Endpoint};

    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_PIPE_BUSY: i32 = 231;

    fn name(endpoint: &Endpoint) -> String {
        format!(r"\\.\pipe\alef-instance-{}", endpoint.key)
    }

    pub(super) struct Listener {
        name: String,
        server: NamedPipeServer,
    }

    impl Listener {
        pub(super) async fn accept(&mut self) -> io::Result<Connection> {
            self.server.connect().await?;
            let next = ServerOptions::new()
                .reject_remote_clients(true)
                .create(&self.name)?;
            Ok(Box::new(std::mem::replace(&mut self.server, next)))
        }
    }

    pub(super) async fn claim(endpoint: &Endpoint) -> io::Result<Claim> {
        let name = name(endpoint);
        match ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)
        {
            Ok(server) => Ok(Claim::First(Listener { name, server })),
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) => Ok(Claim::Taken),
            Err(error) => Err(error),
        }
    }

    pub(super) async fn connect(endpoint: &Endpoint) -> io::Result<Connection> {
        let name = name(endpoint);
        loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => return Ok(Box::new(client)),
                // Every instance of the pipe has a client: wait for the next one to be made.
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::registry::{
        host::{HostFuture, Theme},
        window::UiCall,
    };
    use serde_json::Value;

    #[derive(Default)]
    struct Recorder {
        events: std::sync::Mutex<Vec<(Option<u64>, String, Value)>>,
    }

    impl Host for Recorder {
        fn quit(&self, _code: i32) {}
        fn theme(&self) -> Theme {
            Theme::Light
        }
        fn ui(&self, _caller: u64, _call: UiCall) -> HostFuture {
            Box::pin(async { Ok(Value::Null) })
        }
        fn emit(&self, window: Option<u64>, name: &str, payload: Value) {
            self.events
                .lock()
                .unwrap()
                .push((window, name.to_owned(), payload));
        }
    }

    impl Recorder {
        fn count(&self) -> usize {
            self.events.lock().unwrap().len()
        }
    }

    fn endpoint(key: &str) -> (tempfile::TempDir, Endpoint) {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = Endpoint {
            folder: directory.path().join("instance"),
            key: format!(
                "test-{key}-{:x}",
                fnv1a(&directory.path().to_string_lossy())
            ),
        };
        (directory, endpoint)
    }

    async fn first(endpoint: &Endpoint, host: &Arc<Recorder>) -> Instance {
        let instance = Instance::new(endpoint.clone(), ParsedArgs::default(), host.clone());
        assert!(instance.request().await.unwrap(), "the endpoint was free");
        instance
    }

    /// What the first instance says to somebody who sent `message`.
    async fn talk(endpoint: &Endpoint, message: &[u8]) -> Vec<u8> {
        let mut connection = platform::connect(endpoint).await.unwrap();
        let mut reply = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            let _ = connection.write_all(message).await;
            let _ = connection.flush().await;
            let _ = connection.shutdown().await;
            let _ = connection.read_to_end(&mut reply).await;
        })
        .await;
        reply
    }

    #[tokio::test]
    async fn what_is_not_a_hello_raises_no_event_and_does_not_stop_the_endpoint() {
        let (_directory, endpoint) = endpoint("garbage");
        let host = Arc::new(Recorder::default());
        let _first = first(&endpoint, &host).await;
        let too_many = {
            let args = ParsedArgs {
                raw: vec!["x".to_owned(); MAX_ARGUMENTS + 1],
                ..ParsedArgs::default()
            };
            let hello = Hello {
                args,
                cwd: "/".to_owned(),
            };
            let mut text = serde_json::to_vec(&hello).unwrap();
            text.push(b'\n');
            text
        };
        for message in [
            Vec::new(),
            b"not json\n".to_vec(),
            b"{\"args\": 1}\n".to_vec(),
            b"[]\n".to_vec(),
            vec![b'x'; MAX_MESSAGE as usize + 10],
            too_many,
        ] {
            let reply = talk(&endpoint, &message).await;
            assert_ne!(reply, b"ok\n", "no hello, no thanks");
        }
        assert_eq!(host.count(), 0, "nothing of that was an announcement");

        let later_host = Arc::new(Recorder::default());
        let later = Instance::new(endpoint.clone(), ParsedArgs::default(), later_host);
        assert!(
            !later.request().await.unwrap(),
            "a real later instance is still heard"
        );
        for _ in 0..200 {
            if host.count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(host.count(), 1);
    }

    #[test]
    fn the_socket_goes_to_the_first_folder_where_its_path_fits() {
        let short = PathBuf::from("/run/user/1000/alef");
        let long = PathBuf::from(format!("/home/{}/.cache/app/instance", "x".repeat(120)));
        let name = "0123456789abcdef.sock";
        assert_eq!(
            folder_that_fits(name, vec![short.clone(), long.clone()]),
            short
        );
        assert_eq!(
            folder_that_fits(name, vec![long.clone(), short.clone()]),
            short,
            "the long one is passed over"
        );
        assert_eq!(
            folder_that_fits(
                name,
                vec![long.clone(), PathBuf::from(format!("/{}", "y".repeat(110)))]
            ),
            PathBuf::from(format!("/{}", "y".repeat(110))),
            "when nothing fits the last one is left to say why"
        );
        assert_eq!(folder_that_fits(name, Vec::new()), PathBuf::new());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_socket_left_by_a_dead_process_is_taken_over() {
        let (_directory, endpoint) = endpoint("stale");
        std::fs::create_dir_all(&endpoint.folder).unwrap();
        let path = platform::socket(&endpoint);
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "the file outlives its listener");
        let host = Arc::new(Recorder::default());
        first(&endpoint, &host).await;
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn an_endpoint_nobody_answers_is_reported_not_taken_over() {
        let (_directory, endpoint) = endpoint("deaf");
        std::fs::create_dir_all(&endpoint.folder).unwrap();
        let _deaf = std::os::unix::net::UnixListener::bind(platform::socket(&endpoint)).unwrap();
        let host = Arc::new(Recorder::default());
        let instance = Instance::new(endpoint, ParsedArgs::default(), host);
        let error = instance.request().await.unwrap_err();
        assert_eq!(error.code, ErrorCode::NotAvailable);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_folder_and_the_socket_are_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let (_directory, endpoint) = endpoint("private");
        let host = Arc::new(Recorder::default());
        first(&endpoint, &host).await;
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&endpoint.folder), 0o700);
        assert_eq!(mode(&platform::socket(&endpoint)), 0o600);
    }
}
