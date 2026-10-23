//! Pod streams: container logs, interactive exec and port-forward (#17).

use std::net::Ipv4Addr;
use std::sync::Arc;

use futures::{AsyncBufReadExt, SinkExt as _, Stream, StreamExt as _};
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use kube::api::{AttachParams, LogParams, TerminalSize};
use kube::{Api, Client};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

/// How much history a log view starts from before following.
pub const LOG_TAIL_LINES: i64 = 1000;

/// `kubectl logs -f --tail=LOG_TAIL_LINES -c container`: the stream yields
/// raw byte chunks as the server sends them, ANSI sequences intact.
pub async fn logs(
    client: &Client,
    namespace: &str,
    name: &str,
    container: &str,
) -> kube::Result<impl Stream<Item = std::io::Result<Vec<u8>>> + Send + use<>> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let params = LogParams {
        container: Some(container.to_string()),
        follow: true,
        tail_lines: Some(LOG_TAIL_LINES),
        ..LogParams::default()
    };
    let reader = Box::pin(api.log_stream(name, &params).await?);
    Ok(futures::stream::try_unfold(
        reader,
        |mut reader| async move {
            let chunk = reader.fill_buf().await?.to_vec();
            if chunk.is_empty() {
                return Ok(None);
            }
            reader.consume_unpin(chunk.len());
            Ok(Some((chunk, reader)))
        },
    ))
}

/// What the user sends an exec session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecInput {
    /// Keystrokes and pastes, already terminal-encoded.
    Bytes(Vec<u8>),
    /// The local grid's new size.
    Resize { cols: u16, rows: u16 },
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error(transparent)]
    Connect(#[from] kube::Error),
    #[error("exec stream: {0}")]
    Stream(#[from] std::io::Error),
    /// The server ended the command with a failure other than an exit code.
    #[error("{0}")]
    Failed(String),
}

/// `kubectl exec -it -c container -- command`: pumps `input` to the
/// remote TTY and its output to `output` until the remote side closes,
/// then resolves with the command's exit code. When `input` ends, the
/// remote stdin is closed (EOF) and the output is still read to the end.
pub async fn exec(
    client: &Client,
    namespace: &str,
    name: &str,
    container: &str,
    command: &[String],
    input: impl Stream<Item = ExecInput> + Unpin,
    mut output: impl FnMut(Vec<u8>),
) -> Result<u8, ExecError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let params = AttachParams::interactive_tty().container(container);
    let mut process = api.exec(name, command.to_vec(), &params).await?;
    let attached = "interactive_tty attaches stdin, stdout and a TTY";
    let mut stdin = Some(process.stdin().expect(attached));
    let mut stdout = process.stdout().expect(attached);
    let mut resize = process.terminal_size().expect(attached);
    let status = process.take_status().expect(attached);

    let mut input = input;
    let mut buffer = vec![0; 8192];
    loop {
        tokio::select! {
            read = stdout.read(&mut buffer) => match read? {
                0 => break,
                read => output(buffer[..read].to_vec()),
            },
            next = input.next(), if stdin.is_some() => match next {
                Some(ExecInput::Bytes(bytes)) => {
                    let writer = stdin.as_mut().expect("guarded");
                    writer.write_all(&bytes).await?;
                    writer.flush().await?;
                }
                Some(ExecInput::Resize { cols, rows }) => {
                    // A closed resize channel means the session is ending;
                    // the read side reports how.
                    let _ = resize.send(TerminalSize { width: cols, height: rows }).await;
                }
                None => stdin = None,
            },
        }
    }
    exit_code(status.await.as_ref()).map_err(ExecError::Failed)
}

/// The exit code a remotecommand v4 status reports, as client-go's
/// `errorDecoderV4` reads it; no status at all is success.
pub fn exit_code(status: Option<&Status>) -> Result<u8, String> {
    let Some(status) = status else {
        return Ok(0);
    };
    match status.status.as_deref() {
        Some("Success") => Ok(0),
        Some("Failure") if status.reason.as_deref() == Some("NonZeroExitCode") => {
            let details = status
                .details
                .as_ref()
                .ok_or("error stream protocol error: details must be set")?;
            let cause = details
                .causes
                .iter()
                .flatten()
                .find(|cause| cause.reason.as_deref() == Some("ExitCode"))
                .ok_or("error stream protocol error: no ExitCode cause given")?;
            let code = cause.message.as_deref().unwrap_or_default();
            code.parse().map_err(|_| {
                format!("error stream protocol error: invalid exit code value {code:?}")
            })
        }
        Some("Failure") => Err(status.message.clone().unwrap_or_default()),
        _ => Err("error stream protocol error: unknown error".to_string()),
    }
}

/// A listener on `localhost:port`, or on a free port when that one cannot
/// be bound (taken, or privileged).
pub async fn bind_local(port: u16) -> std::io::Result<TcpListener> {
    match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
        Ok(listener) => Ok(listener),
        Err(_) => TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await,
    }
}

/// `kubectl port-forward pod/name local:port` over a listener the caller
/// bound: each accepted connection gets its own forward to `port`, and a
/// connection's failure goes to `on_error` while the listener keeps
/// accepting. Runs until the listener fails; dropping the future closes
/// every open connection.
pub async fn port_forward(
    client: &Client,
    namespace: &str,
    name: &str,
    port: u16,
    listener: TcpListener,
    on_error: impl Fn(String) + Send + Sync + 'static,
) -> std::io::Result<()> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let on_error = Arc::new(on_error);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (local, _) = accepted?;
                let (api, name, on_error) = (api.clone(), name.to_string(), on_error.clone());
                connections.spawn(async move {
                    if let Err(error) = forward(&api, &name, port, local).await {
                        on_error(error);
                    }
                });
            }
            Some(_) = connections.join_next() => {}
        }
    }
}

/// One local connection's forward, until either side closes.
async fn forward(
    api: &Api<Pod>,
    name: &str,
    port: u16,
    mut local: TcpStream,
) -> Result<(), String> {
    let mut forwarder = api
        .portforward(name, &[port])
        .await
        .map_err(|error| error.to_string())?;
    let requested = "the forwarder carries the requested port";
    let mut remote = forwarder.take_stream(port).expect(requested);
    let remote_error = forwarder.take_error(port).expect(requested);
    let copied = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
    drop(remote);
    forwarder.join().await.map_err(|error| error.to_string())?;
    // The server's own report (e.g. nothing listening on the port) says
    // more than the copy's broken pipe.
    if let Some(error) = remote_error.await {
        return Err(error);
    }
    copied.map(|_| ()).map_err(|error| error.to_string())
}
