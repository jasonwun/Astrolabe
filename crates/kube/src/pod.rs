//! Pod streams: container logs and interactive exec (#17).

use futures::{AsyncBufReadExt, SinkExt as _, Stream, StreamExt as _};
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use kube::api::{AttachParams, LogParams, TerminalSize};
use kube::{Api, Client};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

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
