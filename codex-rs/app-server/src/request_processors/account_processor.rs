use super::bedrock_auth::clear_user_model_provider_if_bedrock;
use super::*;
use crate::model_catalog::ModelCatalog;
use crate::outgoing_message::AccountNotification;

#[derive(Clone)]
pub(crate) struct AccountRequestProcessor {
    auth_manager: Arc<AuthManager>,
    thread_manager: Arc<ThreadManager>,
    outgoing: Arc<OutgoingMessageSender>,
    config: Arc<Config>,
    config_manager: ConfigManager,
    model_catalog: Arc<ModelCatalog>,
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
        let processor = Arc::new(Self {
            auth_manager,
            thread_manager,
            outgoing,
            config,
            config_manager,
            model_catalog,
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
        _params: GetAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Ok(Some(self.read_account().await?.into()))
    }

    async fn read_account(&self) -> Result<GetAccountResponse, JSONRPCErrorError> {
        let config = self.load_latest_config().await;
        let provider = create_model_provider(
            config.model_provider.clone(),
            Some(Arc::clone(&self.auth_manager)),
        );
        let account_state = provider
            .account_state()
            .map_err(|error| internal_error(error.to_string()))?;
        Ok(GetAccountResponse {
            account: account_state.account.map(|_| Account::ApiKey {}),
            requires_openai_auth: account_state.requires_openai_auth,
            workspace_routing: None,
        })
    }

    fn current_account_updated_notification(&self) -> AccountUpdatedNotification {
        let auth = self.auth_manager.auth_cached();
        AccountUpdatedNotification {
            auth_mode: auth.as_ref().map(|_| AuthMode::ApiKey),
            plan_type: None,
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
        match params {
            LoginAccountParams::ApiKey { api_key } => {
                self.login_api_key_v2(request_id, LoginApiKeyParams { api_key })
                    .await;
            }
        }
        Ok(())
    }

    async fn login_api_key_common(
        &self,
        params: &LoginApiKeyParams,
    ) -> std::result::Result<(), JSONRPCErrorError> {
        if !self
            .auth_manager
            .is_login_method_allowed(ForcedLoginMethod::Api)
        {
            return Err(invalid_request("API key login is disabled."));
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
        if payload.success && let Err(error) = self.read_account().await {
            payload.success = false;
            payload.error = Some(error.message);
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
            .map(|_| AuthMode::ApiKey))
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

    async fn get_auth_status_response(
        &self,
        params: GetAuthStatusParams,
    ) -> Result<GetAuthStatusResponse, JSONRPCErrorError> {
        let include_token = params.include_token.unwrap_or(false);

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
            match self.auth_manager.auth().await {
                Some(auth) => {
                    let auth_mode = AuthMode::ApiKey;
                    let token = if include_token {
                        match auth.get_token() {
                            Ok(token) if !token.is_empty() => Some(token),
                            Ok(_) => None,
                            Err(err) => {
                                tracing::warn!("failed to get token for auth status: {err}");
                                None
                            }
                        }
                    } else {
                        None
                    };
                    GetAuthStatusResponse {
                        auth_method: Some(auth_mode),
                        auth_token: token,
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
    let config = config.unwrap_or_else(|_| fallback_config.clone());

    match auth_manager.logout().await {
        Ok(_) => {}
        Err(err) => {
            return Err(internal_error(format!("logout failed: {err}")));
        }
    }
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
