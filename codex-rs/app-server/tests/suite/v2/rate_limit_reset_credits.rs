use std::path::Path;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditParams;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::LoginAccountResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(/*secs*/ 10);
const RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR: &str =
    "CODEX_TEST_RATE_LIMIT_RESET_REQUEST_TIMEOUT_MS";
const SERVER_TIMEOUT_READ_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(/*secs*/ 15);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const INTERNAL_ERROR_CODE: i64 = -32603;

#[tokio::test]
async fn consume_rate_limit_reset_credit_requires_chatgpt_auth() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = initialized_app_server(codex_home.path()).await?;

    let consume_id = mcp
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                idempotency_key: "request-1".to_string(),
                credit_id: None,
            },
        )
        .await?;
    let consume_error = read_error_response(&mut mcp, consume_id).await?;
    assert_eq!(consume_error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(
        consume_error.error.message,
        "codex account authentication required for rate limit reset credits"
    );

    login_with_api_key(&mut mcp, "sk-test-key").await?;
    let consume_id = send_consume_reset_credit(&mut mcp, "request-2").await?;
    let consume_error = read_error_response(&mut mcp, consume_id).await?;
    assert_eq!(consume_error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(
        consume_error.error.message,
        "chatgpt authentication required for rate limit reset credits"
    );
    Ok(())
}

async fn initialized_app_server(codex_home: &Path) -> Result<TestAppServer> {
    TestAppServer::builder()
        .with_codex_home(codex_home)
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await
}

async fn send_consume_reset_credit(mcp: &mut TestAppServer, idempotency_key: &str) -> Result<i64> {
    mcp.send_consume_account_rate_limit_reset_credit_request(
        ConsumeAccountRateLimitResetCreditParams {
            idempotency_key: idempotency_key.to_string(),
            credit_id: None,
        },
    )
    .await
}

async fn read_error_response(mcp: &mut TestAppServer, request_id: i64) -> Result<JSONRPCError> {
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    Ok(error)
}

async fn login_with_api_key(mcp: &mut TestAppServer, api_key: &str) -> Result<()> {
    let request_id = mcp.send_login_account_api_key_request(api_key).await?;
    assert_eq!(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_response::<LoginAccountResponse>(request_id),
        )
        .await??,
        LoginAccountResponse::ApiKey {}
    );
    Ok(())
}
