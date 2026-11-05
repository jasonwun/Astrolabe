//! Pod streams: container logs, interactive exec and port-forward (#17).
//! Each is requested through [`Cluster`](crate::cluster::Cluster) and
//! answered with a handle here (#158, #159); a Service forward resolves
//! its Pod in [`service`](crate::service).

use std::net::Ipv4Addr;
use std::sync::Arc;

use futures::channel::mpsc;
use futures::{AsyncBufReadExt, SinkExt as _, Stream, StreamExt as _};
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use kube::api::{AttachParams, LogParams, TerminalSize};
use kube::{Api, Client};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{AbortHandle, JoinSet};

use crate::cluster::ClusterError;
use crate::object::ObjectKey;

/// How much history a log view starts from before following.
pub const LOG_TAIL_LINES: i64 = 1000;

/// Follow `container`'s log in Pod `pod`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRequest {
    pub pod: ObjectKey,
    pub container: String,
}

/// The task behind a handle, aborted when the handle is dropped; none for
/// a handle built from a channel alone.
#[derive(Default)]
struct AbortOnDrop(Option<AbortHandle>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(work) = self.0.take() {
            work.abort();
        }
    }
}

/// A piece of a log stream, or why the stream stopped.
pub type LogChunk = Result<Vec<u8>, ClusterError>;

/// A followed container log; dropping it stops the stream.
pub struct LogStream {
    chunks: mpsc::UnboundedReceiver<LogChunk>,
    _work: AbortOnDrop,
}

impl LogStream {
    /// A stream of the chunks sent on `chunks`; it ends once their sender
    /// is dropped.
    pub fn new(chunks: mpsc::UnboundedReceiver<LogChunk>) -> Self {
        Self {
            chunks,
            _work: AbortOnDrop::default(),
        }
    }

    /// The stream, owning the task that feeds it.
    pub(crate) fn with_work(chunks: mpsc::UnboundedReceiver<LogChunk>, work: AbortHandle) -> Self {
        Self {
            chunks,
            _work: AbortOnDrop(Some(work)),
        }
    }

    /// The next chunk; `None` once the server closed the stream.
    pub async fn next(&mut self) -> Option<LogChunk> {
        self.chunks.next().await
    }
}

/// Run `shell` interactively in `container` of Pod `pod`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecRequest {
    pub pod: ObjectKey,
    pub container: String,
    pub shell: String,
}

/// What an exec session sends back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecOutput {
    /// Remote TTY output.
    Bytes(Vec<u8>),
    /// The command's exit code, or why the session failed; the last item.
    Exited(Result<u8, ClusterError>),
}

/// A running `kubectl exec -it`; dropping it ends the session.
pub struct ExecSession {
    input: mpsc::UnboundedSender<ExecInput>,
    output: mpsc::UnboundedReceiver<ExecOutput>,
    _work: AbortOnDrop,
}

impl ExecSession {
    /// A session whose keystrokes and resizes go to `input` and whose
    /// output is what is sent on `output`; it ends once that sender is
    /// dropped.
    pub fn new(
        input: mpsc::UnboundedSender<ExecInput>,
        output: mpsc::UnboundedReceiver<ExecOutput>,
    ) -> Self {
        Self {
            input,
            output,
            _work: AbortOnDrop::default(),
        }
    }

    /// The session, owning the task that runs it.
    pub(crate) fn with_work(
        input: mpsc::UnboundedSender<ExecInput>,
        output: mpsc::UnboundedReceiver<ExecOutput>,
        work: AbortHandle,
    ) -> Self {
        Self {
            input,
            output,
            _work: AbortOnDrop(Some(work)),
        }
    }

    /// Where keystrokes and resizes go; closing it sends EOF.
    pub fn input(&self) -> mpsc::UnboundedSender<ExecInput> {
        self.input.clone()
    }

    /// The next output; `None` once the session is gone.
    pub async fn next(&mut self) -> Option<ExecOutput> {
        self.output.next().await
    }
}

/// What a port-forward reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardTarget {
    /// A container port of the Pod (#17).
    Pod(ObjectKey),
    /// A service port of the Service, reaching one of its Pods (#18).
    Service(ObjectKey),
}

/// Forward a localhost port to `port` of `target`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardRequest {
    pub target: ForwardTarget,
    pub port: u16,
}

/// What a port-forward reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardEvent {
    /// Accepting on this localhost port.
    Listening(u16),
    /// One connection's forward failed; the listener goes on.
    ConnectionFailed(ClusterError),
    /// The forward stopped: the client, the bind or the listener failed.
    Failed(ClusterError),
}

/// A running port-forward; dropping it closes the listener and its
/// connections.
pub struct PortForward {
    events: mpsc::UnboundedReceiver<ForwardEvent>,
    _work: AbortOnDrop,
}

impl PortForward {
    /// A forward whose reports are what is sent on `events`; it ends once
    /// their sender is dropped.
    pub fn new(events: mpsc::UnboundedReceiver<ForwardEvent>) -> Self {
        Self {
            events,
            _work: AbortOnDrop::default(),
        }
    }

    /// The forward, owning the task that runs it.
    pub(crate) fn with_work(
        events: mpsc::UnboundedReceiver<ForwardEvent>,
        work: AbortHandle,
    ) -> Self {
        Self {
            events,
            _work: AbortOnDrop(Some(work)),
        }
    }

    /// The next report; `None` once the forward is gone.
    pub async fn next(&mut self) -> Option<ForwardEvent> {
        self.events.next().await
    }
}

/// `kubectl logs -f --tail=LOG_TAIL_LINES -c container`: the stream yields
/// raw byte chunks as the server sends them, ANSI sequences intact.
///
/// # Errors
///
/// A [`kube::Error`] when the log request fails, e.g. the Pod or container
/// does not exist. A read failure after that ends the stream with an
/// `std::io::Error` item.
pub(crate) async fn logs(
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
pub(crate) enum ExecError {
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
///
/// # Errors
///
/// [`ExecError::Connect`] when the exec request fails,
/// [`ExecError::Stream`] when reading or writing the remote streams fails,
/// and [`ExecError::Failed`] when the server ends the command with a
/// failure that is not an exit code (see [`exit_code`]).
pub(crate) async fn exec(
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

/// A refused exec request keeps its kind (a 403 is
/// [`Forbidden`](crate::cluster::ClusterErrorKind::Forbidden)); a stream
/// or command failure is [`Other`](crate::cluster::ClusterErrorKind::Other).
impl From<ExecError> for ClusterError {
    fn from(error: ExecError) -> Self {
        match error {
            ExecError::Connect(error) => error.into(),
            error => ClusterError::other(error),
        }
    }
}

/// The exit code a remotecommand v4 status reports, as client-go's
/// `errorDecoderV4` reads it; no status at all is success.
///
/// # Errors
///
/// The server's message for a `Failure` that is not `NonZeroExitCode`, and
/// a protocol error for a `NonZeroExitCode` status without a parseable
/// `ExitCode` cause or for a status that is neither `Success` nor `Failure`.
pub(crate) fn exit_code(status: Option<&Status>) -> Result<u8, String> {
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
///
/// # Errors
///
/// The [`std::io::Error`] from binding a free port, once `port` failed too.
pub(crate) async fn bind_local(port: u16) -> std::io::Result<TcpListener> {
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
///
/// # Errors
///
/// The [`std::io::Error`] that ends the run: the listener's `accept`
/// failing. A connection's own failure is not one; it goes to `on_error`.
pub(crate) async fn port_forward(
    client: &Client,
    namespace: &str,
    name: &str,
    port: u16,
    listener: TcpListener,
    on_error: impl Fn(ClusterError) + Send + Sync + 'static,
) -> std::io::Result<()> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let name = name.to_string();
    serve_forwards(listener, on_error, move || {
        let target = Ok((api.clone(), name.clone(), port));
        async move { target }
    })
    .await
}

/// Accept on `listener` until it fails, forwarding each connection to the
/// Pod and port `target` resolves for it; a failure, resolution included,
/// goes to `on_error`. Dropping the future closes every open connection.
///
/// # Errors
///
/// The [`std::io::Error`] from the listener's `accept`, which ends the run.
pub(crate) async fn serve_forwards<T>(
    listener: TcpListener,
    on_error: impl Fn(ClusterError) + Send + Sync + 'static,
    target: impl Fn() -> T,
) -> std::io::Result<()>
where
    T: Future<Output = Result<(Api<Pod>, String, u16), ClusterError>> + Send + 'static,
{
    let on_error = Arc::new(on_error);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (local, _) = accepted?;
                let (target, on_error) = (target(), on_error.clone());
                connections.spawn(async move {
                    let forwarded = match target.await {
                        Ok((api, name, port)) => forward(&api, &name, port, local).await,
                        Err(error) => Err(error),
                    };
                    if let Err(error) = forwarded {
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
) -> Result<(), ClusterError> {
    let mut forwarder = api.portforward(name, &[port]).await?;
    let requested = "the forwarder carries the requested port";
    let mut remote = forwarder.take_stream(port).expect(requested);
    let remote_error = forwarder.take_error(port).expect(requested);
    let copied = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
    drop(remote);
    forwarder.join().await.map_err(ClusterError::other)?;
    // The server's own report (e.g. nothing listening on the port) says
    // more than the copy's broken pipe.
    if let Some(error) = remote_error.await {
        return Err(ClusterError::new(error));
    }
    copied.map(|_| ()).map_err(ClusterError::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic remotecommand v4 status (plausible shape, not captured from a
    /// server).
    fn status(json: &str) -> Status {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_successful_command_exits_zero() {
        assert_eq!(exit_code(Some(&status(r#"{"status":"Success"}"#))), Ok(0));
    }

    #[test]
    fn a_closed_status_channel_is_success() {
        assert_eq!(exit_code(None), Ok(0));
    }

    #[test]
    fn a_non_zero_exit_code_comes_from_its_cause() {
        let failed = status(
            r#"{"status":"Failure","message":"command terminated with non-zero exit code: error executing command [sh -c exit 3], exit code 3","reason":"NonZeroExitCode","details":{"causes":[{"reason":"ExitCode","message":"3"}]}}"#,
        );
        assert_eq!(exit_code(Some(&failed)), Ok(3));
    }

    #[test]
    fn a_non_zero_exit_without_its_cause_is_a_protocol_error() {
        let failed = status(r#"{"status":"Failure","reason":"NonZeroExitCode","details":{}}"#);
        assert_eq!(
            exit_code(Some(&failed)),
            Err("error stream protocol error: no ExitCode cause given".to_string())
        );
        let failed = status(r#"{"status":"Failure","reason":"NonZeroExitCode"}"#);
        assert_eq!(
            exit_code(Some(&failed)),
            Err("error stream protocol error: details must be set".to_string())
        );
        let failed = status(
            r#"{"status":"Failure","reason":"NonZeroExitCode","details":{"causes":[{"reason":"ExitCode","message":"256"}]}}"#,
        );
        assert_eq!(
            exit_code(Some(&failed)),
            Err("error stream protocol error: invalid exit code value \"256\"".to_string())
        );
    }

    #[test]
    fn another_failure_is_its_message() {
        let failed = status(
            r#"{"status":"Failure","message":"executable file not found in $PATH","reason":"InternalError"}"#,
        );
        assert_eq!(
            exit_code(Some(&failed)),
            Err("executable file not found in $PATH".to_string())
        );
    }

    #[test]
    fn an_unknown_status_is_a_protocol_error() {
        assert_eq!(
            exit_code(Some(&status("{}"))),
            Err("error stream protocol error: unknown error".to_string())
        );
    }
}
