use axum::Json;
use axum::extract::{FromRequest, Request};
use nsac_core::error::ErrorCode;

use crate::error::ServerError;
use crate::state::AppState;

pub struct AppJson<T>(pub T);

impl<T> FromRequest<AppState> for AppJson<T>
where
    T: serde::de::DeserializeOwned,
{
    type Rejection = ServerError;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ServerError::with_user_message(
                ErrorCode::InvalidParams,
                "Corpo JSON inválido ou campos obrigatórios ausentes.",
                format!("Invalid JSON payload: status={}", rejection.status()),
            )),
        }
    }
}

pub fn parse_ficha_id(raw: &str) -> Result<u8, ServerError> {
    let trimmed = raw.trim();
    let id = trimmed.parse::<u8>().map_err(|_| {
        ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "O identificador da ficha de avaliação deve ser um número entre 1 e 4.",
            "Invalid ficha-avaliacao path id",
        )
    })?;

    if !(1..=4).contains(&id) {
        return Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            "O identificador da ficha de avaliação deve estar entre 1 e 4.",
            format!("Out-of-range ficha-avaliacao id: {id}"),
        ));
    }

    Ok(id)
}

pub fn parse_query_pairs(raw_query: Option<&str>) -> Vec<(String, String)> {
    raw_query
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_bool_strict(raw: &str, param_name: &str) -> Result<bool, ServerError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(ServerError::with_user_message(
            ErrorCode::InvalidParams,
            format!("Parâmetro booleano '{param_name}' inválido; use true, false, 1 ou 0."),
            format!("Invalid boolean query parameter '{param_name}'"),
        )),
    }
}

#[derive(Debug, Default, Clone)]
pub struct TargetQuery {
    pub account_id: Option<i64>,
    pub discord_id: Option<String>,
    pub refresh: bool,
}

pub fn parse_target_query(pairs: &[(String, String)]) -> Result<TargetQuery, ServerError> {
    let mut target = TargetQuery::default();

    for (key, value) in pairs {
        match key.trim() {
            "account_id" => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(ServerError::with_user_message(
                        ErrorCode::InvalidParams,
                        "O parâmetro 'account_id' não pode ser vazio.",
                        "Empty account_id query param",
                    ));
                }
                let parsed = trimmed.parse::<i64>().map_err(|_| {
                    ServerError::with_user_message(
                        ErrorCode::InvalidParams,
                        "O parâmetro 'account_id' deve ser um número inteiro válido.",
                        "Non-integer account_id query param",
                    )
                })?;
                if parsed <= 0 {
                    return Err(ServerError::with_user_message(
                        ErrorCode::InvalidParams,
                        "O parâmetro 'account_id' deve ser positivo.",
                        "Non-positive account_id query param",
                    ));
                }
                target.account_id = Some(parsed);
            }
            "discord_id" => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(ServerError::with_user_message(
                        ErrorCode::InvalidParams,
                        "O parâmetro 'discord_id' não pode ser vazio.",
                        "Empty discord_id query param",
                    ));
                }
                target.discord_id = Some(trimmed.to_string());
            }
            "refresh" => {
                target.refresh = parse_bool_strict(value, "refresh")?;
            }
            _ => {}
        }
    }

    Ok(target)
}
