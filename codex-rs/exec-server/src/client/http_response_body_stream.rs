//! Local HTTP response-body stream plumbing for the `HttpClient` capability.

use std::pin::Pin;

use bytes::Bytes;
use codex_http_client::HttpError;
use codex_http_client::HttpResponse;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::client::ExecServerError;
use crate::protocol::HTTP_REQUEST_BODY_DELTA_METHOD;
use crate::protocol::HttpRequestBodyDeltaNotification;
use crate::rpc::RpcNotificationSender;

enum HttpResponseBodyStreamInner {
    Local {
        body: Pin<Box<dyn futures::Stream<Item = Result<Bytes, HttpError>> + Send>>,
    },
}

/// Request-scoped stream of body chunks for an HTTP response.
///
/// The initial `http/request` call returns status and headers. This stream then
/// yields the response body until EOF or a terminal error.
pub struct HttpResponseBodyStream {
    inner: HttpResponseBodyStreamInner,
    cancellation: Option<CancellationToken>,
}

impl HttpResponseBodyStream {
    /// Creates an in-memory response stream from pre-buffered chunks.
    ///
    /// This is useful for [`crate::HttpClient`] implementations that already
    /// own the response bytes, including lightweight test clients.
    #[doc(hidden)]
    pub fn from_chunks(chunks: Vec<Vec<u8>>) -> Self {
        let body = futures::stream::iter(
            chunks
                .into_iter()
                .map(|chunk| Ok::<Bytes, HttpError>(chunk.into())),
        );
        Self {
            inner: HttpResponseBodyStreamInner::Local {
                body: Box::pin(body),
            },
            cancellation: None,
        }
    }

    pub(super) fn local(response: HttpResponse) -> Self {
        Self {
            inner: HttpResponseBodyStreamInner::Local {
                body: Box::pin(response.bytes_stream()),
            },
            cancellation: None,
        }
    }

    /// Ends this stream when its owning request authority is revoked.
    pub fn cancel_on(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    /// Receives the next response-body chunk.
    ///
    /// Returns `Ok(None)` at EOF and converts stream-side errors into protocol errors.
    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>, ExecServerError> {
        let Some(cancellation) = self.cancellation.clone() else {
            return self.recv_inner().await;
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Ok(None),
            result = self.recv_inner() => result,
        }
    }

    async fn recv_inner(&mut self) -> Result<Option<Vec<u8>>, ExecServerError> {
        match &mut self.inner {
            HttpResponseBodyStreamInner::Local { body } => match body.next().await {
                Some(chunk) => match chunk {
                    Ok(bytes) => Ok(Some(bytes.to_vec())),
                    Err(error) => Err(ExecServerError::HttpRequest(error.to_string())),
                },
                None => Ok(None),
            },
        }
    }
}

pub(super) async fn send_body_delta(
    notifications: &RpcNotificationSender,
    delta: HttpRequestBodyDeltaNotification,
) -> bool {
    notifications
        .notify(HTTP_REQUEST_BODY_DELTA_METHOD, &delta)
        .await
        .is_ok()
}
