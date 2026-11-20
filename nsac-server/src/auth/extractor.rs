use std::fmt;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use zeroize::Zeroizing;

use crate::auth::scopes::has_scope;
use crate::db::accounts::AccountSummary;
use crate::db::auth::sha256_hex;
use crate::error::ServerError;
use crate::state::{AppState, CachedServiceClient, CachedUserSession};

#[derive(Clone)]
pub struct UserPrincipal {
    pub session_id: i64,
    pub account_id: i64,
    pub raw_token: Zeroizing<String>,
}

impl fmt::Debug for UserPrincipal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserPrincipal")
            .field("session_id", &self.session_id)
            .field("account_id", &self.account_id)
            .field("raw_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ServicePrincipal {
    pub client_id: i64,
    pub name: String,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Principal {
    User(UserPrincipal),
    Service(ServicePrincipal),
}

pub struct DualAuth(pub Principal);

impl FromRequestParts<AppState> for DualAuth {
    type Rejection = ServerError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(service_key_header) = parts.headers.get("x-service-key") {
            let raw_key = service_key_header
                .to_str()
                .map_err(|_| ServerError::unauthorized("Cabeçalho X-Service-Key inválido."))?
                .trim();
            if raw_key.is_empty() {
                return Err(ServerError::unauthorized("Cabeçalho X-Service-Key vazio."));
            }
            let principal = authenticate_service_key(state, raw_key).await?;
            return Ok(Self(Principal::Service(principal)));
        }

        let auth_header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .ok_or_else(|| ServerError::unauthorized("Credenciais de autenticação ausentes."))?;

        let auth_str = auth_header
            .to_str()
            .map_err(|_| ServerError::unauthorized("Cabeçalho Authorization inválido."))?
            .trim();

        let (scheme, credential) = auth_str
            .split_once(char::is_whitespace)
            .ok_or_else(|| ServerError::unauthorized("Formato do cabeçalho Authorization inválido."))?;

        let credential = credential.trim();
        if credential.is_empty() {
            return Err(ServerError::unauthorized("Token ou chave de autenticação vazio."));
        }

        if scheme.eq_ignore_ascii_case("Bearer") {
            let principal = authenticate_bearer_token(state, credential).await?;
            Ok(Self(Principal::User(principal)))
        } else if scheme.eq_ignore_ascii_case("Service") {
            let principal = authenticate_service_key(state, credential).await?;
            Ok(Self(Principal::Service(principal)))
        } else {
            Err(ServerError::unauthorized(
                "Esquema de autenticação não suportado.",
            ))
        }
    }
}

async fn authenticate_bearer_token(
    state: &AppState,
    raw_token: &str,
) -> Result<UserPrincipal, ServerError> {
    let now = state.clock.now();
    let token_hash = sha256_hex(raw_token);

    if let Some(cached) = state.caches.get_user_session(&token_hash).await {
        if cached.expires_at <= now || !cached.account_active {
            state.caches.invalidate_user_session(&token_hash).await;
            return Err(ServerError::unauthorized(
                "Sessão expirada ou conta inativa.",
            ));
        }
        return Ok(UserPrincipal {
            session_id: cached.session_id,
            account_id: cached.account_id,
            raw_token: Zeroizing::new(raw_token.to_string()),
        });
    }

    let lookup = state
        .auth()
        .find_user_session_by_raw_token(raw_token)
        .await?
        .ok_or_else(|| ServerError::unauthorized("Token de sessão inválido."))?;

    if lookup.revoked_at.is_some() || lookup.expires_at <= now || !lookup.account_active {
        return Err(ServerError::unauthorized(
            "Sessão revogada, expirada ou conta inativa.",
        ));
    }

    state
        .caches
        .put_user_session(
            token_hash,
            CachedUserSession {
                session_id: lookup.session_id,
                account_id: lookup.account_id,
                expires_at: lookup.expires_at,
                account_active: lookup.account_active,
            },
        )
        .await;

    Ok(UserPrincipal {
        session_id: lookup.session_id,
        account_id: lookup.account_id,
        raw_token: Zeroizing::new(raw_token.to_string()),
    })
}

async fn authenticate_service_key(
    state: &AppState,
    raw_key: &str,
) -> Result<ServicePrincipal, ServerError> {
    let key_hash = sha256_hex(raw_key);

    if let Some(cached) = state.caches.get_service_client(&key_hash).await {
        if !cached.active {
            state.caches.invalidate_service_client_by_hash(&key_hash).await;
            return Err(ServerError::unauthorized("Cliente de serviço desativado."));
        }
        return Ok(ServicePrincipal {
            client_id: cached.id,
            name: cached.name,
            scopes: cached.scopes,
        });
    }

    let record = state
        .auth()
        .find_service_client_by_raw_key(raw_key)
        .await?
        .ok_or_else(|| ServerError::unauthorized("Chave de serviço inválida."))?;

    if !record.active {
        return Err(ServerError::unauthorized("Cliente de serviço desativado."));
    }

    state
        .caches
        .put_service_client(
            key_hash,
            CachedServiceClient {
                id: record.id,
                name: record.name.clone(),
                scopes: record.scopes.clone(),
                active: record.active,
            },
        )
        .await;

    Ok(ServicePrincipal {
        client_id: record.id,
        name: record.name,
        scopes: record.scopes,
    })
}

#[derive(Debug, Clone)]
pub struct TargetAccount {
    pub account_id: i64,
    pub class_code: Option<String>,
    pub summary: AccountSummary,
}

pub async fn resolve_target_account(
    state: &AppState,
    principal: &Principal,
    required_scope: &str,
    requested_account_id: Option<i64>,
    requested_discord_id: Option<&str>,
) -> Result<TargetAccount, ServerError> {
    let trimmed_discord = match requested_discord_id {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(ServerError::invalid_params(
                    "O parâmetro 'discord_id' não pode ser vazio.",
                ));
            }
            Some(trimmed)
        }
        None => None,
    };

    match principal {
        Principal::User(user) => {
            if let Some(req_id) = requested_account_id
                && req_id != user.account_id
            {
                return Err(ServerError::forbidden(
                    "Token de usuário não pode acessar outra conta.",
                ));
            }

            let summary = state
                .accounts()
                .find_by_id(user.account_id)
                .await?
                .ok_or_else(|| ServerError::unauthorized("Conta do usuário não encontrada."))?;

            if !summary.active {
                return Err(ServerError::unauthorized("Conta desativada."));
            }

            if let Some(req_discord) = trimmed_discord
                && summary.discord_id.as_deref() != Some(req_discord)
            {
                return Err(ServerError::forbidden(
                    "Token de usuário não pode acessar outro discord_id.",
                ));
            }

            Ok(TargetAccount {
                account_id: summary.id,
                class_code: summary.class_code.clone(),
                summary,
            })
        }
        Principal::Service(service) => {
            if !has_scope(&service.scopes, required_scope) {
                return Err(ServerError::forbidden(format!(
                    "Cliente de serviço '{}' não possui o escopo '{}'.",
                    service.name, required_scope
                )));
            }

            let summary = match (requested_account_id, trimmed_discord) {
                (None, None) => {
                    return Err(ServerError::invalid_params(
                        "Chamada de serviço requer 'account_id' ou 'discord_id'.",
                    ));
                }
                (Some(aid), None) => state
                    .accounts()
                    .find_by_id(aid)
                    .await?
                    .ok_or_else(|| {
                        ServerError::user_not_linked(format!(
                            "Conta alvo account_id={aid} não encontrada."
                        ))
                    })?,
                (None, Some(did)) => state
                    .accounts()
                    .find_by_discord_id(did)
                    .await?
                    .ok_or_else(|| {
                        ServerError::user_not_linked(
                            "Nenhuma conta vinculada ao discord_id informado.",
                        )
                    })?,
                (Some(aid), Some(did)) => {
                    let acct = state
                        .accounts()
                        .find_by_id(aid)
                        .await?
                        .ok_or_else(|| {
                            ServerError::user_not_linked(format!(
                                "Conta alvo account_id={aid} não encontrada."
                            ))
                        })?;
                    if acct.discord_id.as_deref() != Some(did) {
                        return Err(ServerError::invalid_params(
                            "Os parâmetros 'account_id' e 'discord_id' não correspondem à mesma conta.",
                        ));
                    }
                    acct
                }
            };

            if !summary.active {
                return Err(ServerError::forbidden(
                    "A conta alvo está desativada.",
                ));
            }

            Ok(TargetAccount {
                account_id: summary.id,
                class_code: summary.class_code.clone(),
                summary,
            })
        }
    }
}
