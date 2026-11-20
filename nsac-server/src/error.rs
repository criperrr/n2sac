use std::fmt;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use nsac_core::error::{AppError, ErrorCode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorItem {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MissingVar {
        name: &'static str,
    },
    InvalidVar {
        name: &'static str,
        reason: &'static str,
    },
    InvalidEncryptionKey,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVar { name } => {
                write!(f, "Missing required environment variable: {name}")
            }
            Self::InvalidVar { name, reason } => {
                write!(f, "Invalid environment variable {name}: {reason}")
            }
            Self::InvalidEncryptionKey => {
                f.write_str("Invalid ENCRYPTION_KEY: expected 64 hex characters (32 bytes)")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

pub struct ServerError {
    code: ErrorCode,
    http_status_override: Option<u16>,
    code_str_override: Option<&'static str>,
    user_message: String,
    internal_message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ServerError {
    pub fn new(code: ErrorCode, internal_message: impl Into<String>) -> Self {
        Self {
            code,
            http_status_override: None,
            code_str_override: None,
            user_message: code.default_user_message().to_string(),
            internal_message: internal_message.into(),
            source: None,
        }
    }

    pub fn with_user_message(
        code: ErrorCode,
        user_message: impl Into<String>,
        internal_message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            http_status_override: None,
            code_str_override: None,
            user_message: user_message.into(),
            internal_message: internal_message.into(),
            source: None,
        }
    }

    pub fn conflict(
        user_message: impl Into<String>,
        internal_message: impl Into<String>,
    ) -> Self {
        Self {
            code: ErrorCode::InvalidParams,
            http_status_override: Some(409),
            code_str_override: Some("CONFLICT"),
            user_message: user_message.into(),
            internal_message: internal_message.into(),
            source: None,
        }
    }

    pub fn invalid_params(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, internal_message)
    }

    pub fn unauthorized(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, internal_message)
    }

    pub fn forbidden(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, internal_message)
    }

    pub fn not_found(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, internal_message)
    }

    pub fn user_not_linked(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UserNotLinked, internal_message)
    }

    pub fn invalid_credentials(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidNsacCredentials, internal_message)
    }

    pub fn auth_expired(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NsacAuthExpired, internal_message)
    }

    pub fn nsac_unavailable(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NsacUnavailable, internal_message)
    }

    pub fn session_refresh(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::SessionRefreshError, internal_message)
    }

    pub fn crypto(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::CryptoError, internal_message)
    }

    pub fn internal(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InternalError, internal_message)
    }

    pub const fn error_code(&self) -> ErrorCode {
        self.code
    }

    pub const fn code_str(&self) -> &'static str {
        match self.code_str_override {
            Some(s) => s,
            None => self.code.as_str(),
        }
    }

    pub const fn http_status(&self) -> u16 {
        match self.http_status_override {
            Some(status) => status,
            None => self.code.http_status(),
        }
    }

    pub fn user_message(&self) -> &str {
        &self.user_message
    }

    pub fn internal_message(&self) -> &str {
        &self.internal_message
    }

    pub fn to_error_item(&self) -> ErrorItem {
        ErrorItem {
            code: self.code_str().to_string(),
            message: self.user_message.clone(),
        }
    }
}

impl fmt::Debug for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerError")
            .field("code", &self.code_str())
            .field("http_status", &self.http_status())
            .field("user_message", &self.user_message)
            .field("internal_message", &self.internal_message)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code_str(), self.user_message)
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

impl From<AppError> for ServerError {
    fn from(err: AppError) -> Self {
        Self {
            code: err.code,
            http_status_override: None,
            code_str_override: None,
            user_message: err.user_message.clone(),
            internal_message: err.internal_message().to_owned(),
            source: Some(Box::new(err)),
        }
    }
}

impl From<ConfigError> for ServerError {
    fn from(err: ConfigError) -> Self {
        let internal = err.to_string();
        Self {
            code: ErrorCode::InternalError,
            http_status_override: None,
            code_str_override: None,
            user_message: "Erro de configuração interna do servidor.".to_string(),
            internal_message: internal,
            source: Some(Box::new(err)),
        }
    }
}

impl From<sqlx::Error> for ServerError {
    fn from(err: sqlx::Error) -> Self {
        if let Some(db_err) = err.as_database_error() {
            let pg_code = db_err.code();
            let constraint = db_err.constraint().unwrap_or("unknown").to_string();
            return match pg_code.as_deref() {
                Some("23505") if constraint == "accounts_discord_id_key" => Self {
                    code: ErrorCode::InvalidParams,
                    http_status_override: Some(409),
                    code_str_override: Some("CONFLICT"),
                    user_message: "Este discord_id já está vinculado a outra conta.".to_string(),
                    internal_message: format!(
                        "PostgreSQL unique violation on constraint {constraint}"
                    ),
                    source: Some(Box::new(err)),
                },
                Some("23505") => Self {
                    code: ErrorCode::InvalidParams,
                    http_status_override: None,
                    code_str_override: None,
                    user_message: "Registro conflitante com dados já existentes.".to_string(),
                    internal_message: format!(
                        "PostgreSQL unique violation on constraint {constraint}"
                    ),
                    source: Some(Box::new(err)),
                },
                Some("23503") => Self {
                    code: ErrorCode::NotFound,
                    http_status_override: None,
                    code_str_override: None,
                    user_message: "Referência de conta ou recurso não encontrada.".to_string(),
                    internal_message: format!(
                        "PostgreSQL foreign key violation on constraint {constraint}"
                    ),
                    source: Some(Box::new(err)),
                },
                Some("23514") => Self {
                    code: ErrorCode::InvalidParams,
                    http_status_override: None,
                    code_str_override: None,
                    user_message: "Dados inválidos para persistência.".to_string(),
                    internal_message: format!(
                        "PostgreSQL check violation on constraint {constraint}"
                    ),
                    source: Some(Box::new(err)),
                },
                other => Self {
                    code: ErrorCode::InternalError,
                    http_status_override: None,
                    code_str_override: None,
                    user_message: "Ocorreu um erro interno ao acessar o banco de dados."
                        .to_string(),
                    internal_message: format!("PostgreSQL database error (code={other:?})"),
                    source: Some(Box::new(err)),
                },
            };
        }

        match err {
            sqlx::Error::RowNotFound => Self {
                code: ErrorCode::NotFound,
                http_status_override: None,
                code_str_override: None,
                user_message: ErrorCode::NotFound.default_user_message().to_string(),
                internal_message: "Database query returned no matching row".to_string(),
                source: Some(Box::new(err)),
            },
            sqlx::Error::PoolTimedOut => Self {
                code: ErrorCode::InternalError,
                http_status_override: None,
                code_str_override: None,
                user_message: "Ocorreu um erro interno ao acessar o banco de dados.".to_string(),
                internal_message: "PostgreSQL connection pool timed out".to_string(),
                source: Some(Box::new(err)),
            },
            sqlx::Error::PoolClosed => Self {
                code: ErrorCode::InternalError,
                http_status_override: None,
                code_str_override: None,
                user_message: "Ocorreu um erro interno ao acessar o banco de dados.".to_string(),
                internal_message: "PostgreSQL connection pool is closed".to_string(),
                source: Some(Box::new(err)),
            },
            other => Self {
                code: ErrorCode::InternalError,
                http_status_override: None,
                code_str_override: None,
                user_message: "Ocorreu um erro interno ao acessar o banco de dados.".to_string(),
                internal_message: "Unexpected database driver error".to_string(),
                source: Some(Box::new(other)),
            },
        }
    }
}

impl From<sqlx::migrate::MigrateError> for ServerError {
    fn from(err: sqlx::migrate::MigrateError) -> Self {
        Self {
            code: ErrorCode::InternalError,
            http_status_override: None,
            code_str_override: None,
            user_message: "Erro ao aplicar migrações do banco de dados.".to_string(),
            internal_message: "PostgreSQL migration failed".to_string(),
            source: Some(Box::new(err)),
        }
    }
}

impl From<tokio::sync::AcquireError> for ServerError {
    fn from(err: tokio::sync::AcquireError) -> Self {
        Self {
            code: ErrorCode::InternalError,
            http_status_override: None,
            code_str_override: None,
            user_message: "Semáforo de concorrência NSAC indisponível.".to_string(),
            internal_message: "Failed to acquire NSAC concurrency semaphore".to_string(),
            source: Some(Box::new(err)),
        }
    }
}

impl From<serde_json::Error> for ServerError {
    fn from(err: serde_json::Error) -> Self {
        Self {
            code: ErrorCode::InternalError,
            http_status_override: None,
            code_str_override: None,
            user_message: "Erro ao processar dados JSON internos.".to_string(),
            internal_message: format!(
                "JSON serialization/deserialization failed at line {}, column {}",
                err.line(),
                err.column()
            ),
            source: Some(Box::new(err)),
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::json!({
            "success": false,
            "errors": [self.to_error_item()],
        });
        (status, Json(body)).into_response()
    }
}
