//! HTTP client capability implementations used by local environments and the server.
//!
//! This module is the facade for the environment-owned [`crate::HttpClient`]
//! capability:
//! - [`RouteAwareHttpClient`] executes requests through the shared transport
//! - [`HttpResponseBodyStream`] presents buffered and streamed local bodies
//!   through one byte-stream API

#[path = "http_response_body_stream.rs"]
pub(crate) mod response_body_stream;
#[path = "route_aware_http_client.rs"]
mod route_aware_http_client;

pub use response_body_stream::HttpResponseBodyStream;
pub(crate) use route_aware_http_client::PendingRouteAwareHttpBodyStream;
pub use route_aware_http_client::RouteAwareHttpClient;
pub(crate) use route_aware_http_client::RouteAwareHttpRequestRunner;
