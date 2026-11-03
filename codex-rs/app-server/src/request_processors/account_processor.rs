use super::bedrock_auth::clear_user_model_provider_if_bedrock;
use super::*;
use crate::auth_mode::auth_mode_to_api;
use crate::model_catalog::ModelCatalog;
use crate::outgoing_message::AccountNotification;
use codex_mcp::ema_auth_scope;
use codex_rmcp_client::EnterpriseOAuthCredentialGuard;

mod enterprise_login;

pub(super) use enterprise_login::EnterpriseLoginCompletion;
pub(super) use enterprise_login::EnterpriseLoginTarget;


enum RefreshTokenRequestOutcome {
    NotAttemptedOrSucceeded,
    FailedTransiently,
    FailedPermanently,
}

#[derive(Clone)]
pub(crate) struct AccountRequestProcessor {
    auth_manager: Arc<AuthManager>,
    thread_manager: Arc<ThreadManager>,
    outgoing: Arc<OutgoingMessageSender>,
    config: Arc<Config>,
    config_manager: ConfigManager,
    model_catalog: Arc<ModelCatalog>,
    pub(super) enterprise_login: Arc<enterprise_login::EnterpriseLoginState>,
}

impl AccountRequestProcessor {
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        thread_manager: Arc<ThreadManager>,
        outgoing: Arc<OutgoingMessageSender>,
        config: Arc<Config>,
        config_manager: ConfigManager,
        model_catalog: Arc<ModelCatalog>,
    ) -> Arc<Self> {
        let enterprise_login = Arc::new(enterprise_login::EnterpriseLoginState::new(
            Arc::clone(&auth_manager),
            Arc::clone(&thread_manager),
            config_manager.clone(),
        ));
        let processor = Arc::new(Self {
            auth_manager,
            thread_manager,
            outgoing,
            config,
            config_manager,
            model_catalog,
            enterprise_login,
        });
        processor
    }

    pub(crate) async fn login_account(
        &self,
        request_id: ConnectionRequestId,
        params: LoginAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.login_v2(request_id, params).await.map(|()| None)
    }

    pub(crate) async fn logout_account(
        &self,
        request_id: ConnectionRequestId,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.logout_v2(request_id).await.map(|()| None)
    }

    pub(crate) async fn cancel_login_account(
        &self,
        _params: CancelLoginAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        // API-key login completes synchronously, so there is never an active login to cancel.
        Ok(Some(
            CancelLoginAccountResponse {
                status: CancelLoginAccountStatus::NotFound,
            }
            .into(),
        ))
    }

    pub(crate) async fn get_auth_status(
        &self,
        params: GetAuthStatusParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_auth_status_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_account(
        &self,
        params: GetAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.refresh_token_if_requested(params.refresh_token)
            .await;
        let config = self.load_latest_config().await;
        let provider = create_model_provider(
            config.model_provider.clone(),
            Some(Arc::clone(&self.auth_manager)),
        );
        let account_state = provider
            .account_state()
            .map_err(|error| internal_error(error.to_string()))?;
        Ok(Some(
            GetAccountResponse {
                account: account_state.account.map(Account::from),
                requires_openai_auth: account_state.requires_openai_auth,
                workspace_routing: None,
            }
            .into(),
        ))
    }

    pub(crate) async fn cancel_active_login(&self) {
        self.enterprise_login.cancel(/*login_id*/ None).await;
    }

    pub(crate) fn clear_external_auth(&self) {
        self.auth_manager.clear_external_auth();
    }

    fn current_account_updated_notification(&self) -> AccountUpdatedNotification {
        let auth = self.auth_manager.auth_cached();
        AccountUpdatedNotification {
            auth_mode: auth
                .as_ref()
                .map(CodexAuth::api_auth_mode)
                .map(auth_mode_to_api),
            plan_type: auth.as_ref().and_then(CodexAuth::account_plan_type),
        }
    }

    async fn load_latest_config(&self) -> Config {
        match self
            .config_manager
            .load_latest_config(/*fallback_cwd*/ None)
            .await
        {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!("failed to reload config, using startup config: {err}");
                self.config.as_ref().clone()
            }
        }
    }

    async fn refresh_skills_and_mcp_for_current_config(
        config_manager: &ConfigManager,
        thread_manager: &Arc<ThreadManager>,
    ) {
        thread_manager.skills_service().clear_cache();
        crate::mcp_refresh::reload_mcp_config_best_effort(thread_manager, config_manager).await;
        thread_manager.invalidate_mcp_runtimes().await;
    }

    async fn login_v2(
        &self,
        request_id: ConnectionRequestId,
        params: LoginAccountParams,
    ) -> Result<(), JSONRPCErrorError> {
        self.enterprise_login.cancel(/*login_id*/ None).await;
        match params {
            LoginAccountParams::ApiKey { api_key } => {
                self.login_api_key_v2(request_id, LoginApiKeyParams { api_key })
                    .await;
            }
            LoginAccountParams::Chatgpt { .. }
            | LoginAccountParams::ChatgptDeviceCode
            | LoginAccountParams::ChatgptAuthTokens { .. }
            | LoginAccountParams::AmazonBedrock { .. }
            | LoginAccountParams::AmazonBedrockAccessKeys { .. } => {
                self.outgoing
                    .send_result(
                        request_id,
                        Err::<LoginAccountResponse, _>(invalid_request(
                            "Only API key login is supported.",
                        )),
                    )
                    .await;
            }
        }
        Ok(())
    }

    fn external_auth_active_error(&self) -> JSONRPCErrorError {
        invalid_request(
            "External auth is active. Use account/login/start (chatgptAuthTokens) to update it or account/logout to clear it.",
        )
    }

    async fn login_api_key_common(
        &self,
        params: &LoginApiKeyParams,
    ) -> std::result::Result<(), JSONRPCErrorError> {
        if self.auth_manager.is_external_chatgpt_auth_active() {
            return Err(self.external_auth_active_error());
        }

        if !self
            .auth_manager
            .is_login_method_allowed(ForcedLoginMethod::Api)
        {
            return Err(invalid_request(
                "API key login is disabled. Use ChatGPT login instead.",
            ));
        }

        match login_with_api_key(
            &self.config.codex_home,
            &params.api_key,
            self.config.cli_auth_credentials_store_mode,
            self.config.auth_keyring_backend_kind(),
        ) {
            Ok(()) => {
                self.auth_manager.reload().await;
                self.config_manager.clear_chatgpt_config_loader();
                Ok(())
            }
            Err(err) => Err(internal_error(format!("failed to save api key: {err}"))),
        }
    }

    async fn login_api_key_v2(&self, request_id: ConnectionRequestId, params: LoginApiKeyParams) {
        let result = self
            .login_api_key_common(&params)
            .await
            .map(|()| LoginAccountResponse::ApiKey {});
        let logged_in = result.is_ok();
        if logged_in {
            self.model_catalog.invalidate().await;
        }
        self.outgoing.send_result(request_id, result).await;

        if logged_in {
            self.send_login_success_notifications(/*login_id*/ None)
                .await;
        }
    }

    async fn send_login_success_notifications(&self, login_id: Option<Uuid>) {
        self.thread_manager.invalidate_mcp_runtimes().await;
        self.send_account_login_notifications(AccountLoginCompletedNotification {
            login_id: login_id.map(|id| id.to_string()),
            success: true,
            error: None,
            onboarding_entrypoint: None,
        })
        .await;
    }

    async fn send_account_login_notifications(
        &self,
        mut payload: AccountLoginCompletedNotification,
    ) {
        let auth_changes = self.auth_manager.auth_change_state_receiver();
        let owner_generation = auth_changes.borrow().owner_generation;
        if payload.success
            && let Err(error) = self.read_account(/*request*/ None).await
        {
            payload.success = false;
            payload.error = Some(error.to_string());
        }
        if payload.success && auth_changes.borrow().owner_generation == owner_generation {
            Self::refresh_skills_and_mcp_for_current_config(
                &self.config_manager,
                &self.thread_manager,
            )
            .await;
        }

        let success = payload.success;
        self.outgoing
            .send_account_notification(
                /*connection_id*/ None,
                &auth_changes,
                owner_generation,
                AccountNotification::LoginCompleted(payload),
            )
            .await;

        if success {
            let notification = self.current_account_updated_notification();
            self.outgoing
                .send_account_notification(
                    /*connection_id*/ None,
                    &auth_changes,
                    owner_generation,
                    AccountNotification::Updated(notification),
                )
                .await;
        }
    }

    async fn logout_common(&self) -> std::result::Result<Option<AuthMode>, JSONRPCErrorError> {
        self.cancel_active_login().await;
        let config =
            logout_configured_account(&self.config_manager, &self.auth_manager, &self.config)
                .await?;
        self.thread_manager.invalidate_mcp_runtimes().await;
        clear_account_configuration(&self.config_manager, &config).await?;

        self.model_catalog.invalidate().await;

        Self::refresh_skills_and_mcp_for_current_config(
            &self.config_manager,
            &self.thread_manager,
        )
        .await;

        // Reflect the current auth method after logout (likely None).
        Ok(self
            .auth_manager
            .auth_cached()
            .as_ref()
            .map(CodexAuth::api_auth_mode)
            .map(auth_mode_to_api))
    }

    async fn logout_v2(&self, request_id: ConnectionRequestId) -> Result<(), JSONRPCErrorError> {
        let result = self.logout_common().await;
        let account_updated =
            result
                .as_ref()
                .ok()
                .cloned()
                .map(|auth_mode| AccountUpdatedNotification {
                    auth_mode,
                    plan_type: None,
                });
        self.outgoing
            .send_result(request_id, result.map(|_| LogoutAccountResponse {}))
            .await;

        if let Some(payload) = account_updated {
            self.outgoing
                .send_server_notification(ServerNotification::AccountUpdated(payload))
                .await;
        }
        Ok(())
    }

    async fn refresh_token_if_requested(&self, do_refresh: bool) -> RefreshTokenRequestOutcome {
        if self.auth_manager.is_external_chatgpt_auth_active() {
            return RefreshTokenRequestOutcome::NotAttemptedOrSucceeded;
        }
        if do_refresh && let Err(err) = self.auth_manager.refresh_token().await {
            let failed_reason = err.failed_reason();
            if failed_reason.is_none() {
                tracing::warn!("failed to refresh token while getting account: {err}");
                return RefreshTokenRequestOutcome::FailedTransiently;
            }
            return RefreshTokenRequestOutcome::FailedPermanently;
        }
        RefreshTokenRequestOutcome::NotAttemptedOrSucceeded
    }

    async fn get_auth_status_response(
        &self,
        params: GetAuthStatusParams,
    ) -> Result<GetAuthStatusResponse, JSONRPCErrorError> {
        let include_token = params.include_token.unwrap_or(false);
        let do_refresh = params.refresh_token.unwrap_or(false);

        self.refresh_token_if_requested(do_refresh).await;

        // Determine whether auth is required based on the active model provider.
        // If a custom provider is configured with `requires_openai_auth == false`,
        // then no auth step is required; otherwise, default to requiring auth.
        let config = self.load_latest_config().await;
        let requires_openai_auth = config.model_provider.requires_openai_auth;

        let response = if !requires_openai_auth {
            GetAuthStatusResponse {
                auth_method: None,
                auth_token: None,
                requires_openai_auth: Some(false),
            }
        } else {
            let auth = if do_refresh {
                self.auth_manager.auth_cached()
            } else {
                self.auth_manager.auth().await
            };
            match auth {
                Some(auth) => {
                    let permanent_refresh_failure =
                        self.auth_manager.refresh_failure_for_auth(&auth).is_some();
                    let auth_mode = auth_mode_to_api(auth.api_auth_mode());
                    let (reported_auth_method, token_opt) =
                        if matches!(
                            auth,
                            CodexAuth::Headers(_) | CodexAuth::PersonalAccessToken(_)
                        ) || include_token && permanent_refresh_failure
                        {
                            // Host-owned and metadata-bearing credentials are never exported.
                            (Some(auth_mode), None)
                        } else {
                            match auth.get_token() {
                                Ok(token) if !token.is_empty() => {
                                    let tok = if include_token { Some(token) } else { None };
                                    (Some(auth_mode), tok)
                                }
                                Ok(_) => (None, None),
                                Err(err) => {
                                    tracing::warn!("failed to get token for auth status: {err}");
                                    (None, None)
                                }
                            }
                        };
                    GetAuthStatusResponse {
                        auth_method: reported_auth_method,
                        auth_token: token_opt,
                        requires_openai_auth: Some(true),
                    }
                }
                None => GetAuthStatusResponse {
                    auth_method: None,
                    auth_token: None,
                    requires_openai_auth: Some(true),
                },
            }
        };

        Ok(response)
    }

}

/// Remove account credentials using the current account's policy, even during startup recovery.
pub(crate) async fn logout_configured_account(
    config_manager: &ConfigManager,
    auth_manager: &Arc<AuthManager>,
    fallback_config: &Config,
) -> Result<Config, JSONRPCErrorError> {
    // Another process may have changed the persisted workspace. Reload both
    // account authority and its policy before selecting a grant to remove.
    auth_manager.reload().await;
    config_manager.update_managed_config_loader(Arc::clone(auth_manager), fallback_config);
    let config = config_manager
        .load_latest_config(/*fallback_cwd*/ None)
        .await;
    let scope = ema_auth_scope(auth_manager.auth_cached().as_ref());
    let enterprise_policy_failed = scope.is_some() && config.is_err();
    // Startup policy may belong to another workspace. Never use that fallback
    // to select a credential; a policy failure must not block primary logout.
    let enterprise_grant = config.as_ref().ok().and_then(|config| {
        scope
            .as_ref()
            .zip(config.mcp_enterprise_managed_auth.as_ref())
            .map(|(scope, profile)| {
                (
                    profile.idp.credential_name(scope),
                    profile.idp.issuer.clone(),
                    config.auth_keyring_backend_kind(),
                )
            })
    });
    let config = config.unwrap_or_else(|_| fallback_config.clone());

    // Retain the credential lock through primary logout. Otherwise a second
    // process can commit after deletion but before the account is removed.
    let enterprise_guard =
        if let Some((credential_name, issuer, keyring_backend)) = enterprise_grant {
            EnterpriseOAuthCredentialGuard::acquire(&credential_name, &issuer, keyring_backend)
                .await
                .map(Some)
        } else {
            Ok(None)
        };
    let cleanup_failed = match &enterprise_guard {
        Ok(Some(guard)) => guard.delete_tokens().is_err(),
        Ok(None) => false,
        Err(_) => true,
    };
    if enterprise_policy_failed || cleanup_failed {
        tracing::warn!("Failed to remove enterprise authorization; continuing account logout");
    }

    match auth_manager.logout().await {
        Ok(_) => {}
        Err(err) => {
            return Err(internal_error(format!("logout failed: {err}")));
        }
    }
    drop(enterprise_guard);
    Ok(config)
}

/// Discard account-scoped cloud and provider settings after credentials have been removed.
pub(crate) async fn clear_account_configuration(
    config_manager: &ConfigManager,
    config: &Config,
) -> Result<(), JSONRPCErrorError> {
    config_manager.clear_chatgpt_config_loader();
    if config.model_provider.is_amazon_bedrock() {
        clear_user_model_provider_if_bedrock(config_manager, config).await?;
    }
    Ok(())
}
