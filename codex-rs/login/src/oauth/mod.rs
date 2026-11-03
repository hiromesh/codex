//! OAuth protocol operations shared by independent credential managers.
//!
//! Callers supply an HTTP client with the application's proxy and CA policy. They retain
//! ownership of credential storage, refresh scheduling, account validation, and login UX.

mod client;
mod diagnostics;
mod error;

pub(crate) use client::OAuthClient;
pub(crate) use client::RefreshReason;
pub(crate) use client::RefreshTokenGrant;
pub(crate) use client::TokenEncoding;
pub(crate) use client::TokenEndpoint;
pub(crate) use error::ErrorBodyLimit;
pub(crate) use error::OAuthError;
pub(crate) use error::TokenRejection;
