//! Pod streams: container logs (#17).

use futures::{AsyncBufReadExt, Stream};
use k8s_openapi::api::core::v1::Pod;
use kube::api::LogParams;
use kube::{Api, Client};

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
