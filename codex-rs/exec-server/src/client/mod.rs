//! Local HTTP capability plumbing shared by the standalone server and environments.

pub(crate) mod http_client;

pub(crate) use crate::client_error::ExecServerError;

pub(crate) fn can_retry_connection_attempt(error: &ExecServerError) -> bool {
    error.is_retryable_preparation_error()
}
