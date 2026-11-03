//! The HTTP capability trait exposed by execution environments.

use futures::future::BoxFuture;

use crate::ExecServerError;
use crate::HttpRequestParams;
use crate::HttpRequestResponse;
use crate::HttpResponseBodyStream;

/// Sends HTTP requests through a runtime-selected transport.
///
/// This is the HTTP capability counterpart to [`crate::ExecBackend`]. Callers
/// use it when they need environment-owned network requests but should not
/// depend on the concrete connection type or how that connection is established.
pub trait HttpClient: Send + Sync {
    /// Perform an HTTP request and buffer the response body.
    fn http_request(
        &self,
        params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<HttpRequestResponse, ExecServerError>>;

    /// Perform an HTTP request and return a streamed body handle.
    fn http_request_stream(
        &self,
        params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<(HttpRequestResponse, HttpResponseBodyStream), ExecServerError>>;
}
