use std::fmt;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidNsacCredentials,
    NsacAuthExpired,
    NsacUnavailable,
    UserNotLinked,
    InvalidParams,
    SessionRefreshError,
    CryptoError,
    Unauthorized,
    Forbidden,
    NotFound,
    InternalError,
}

impl ErrorCode {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidNsacCredentials => "INVALID_NSAC_CREDENTIALS",
            Self::NsacAuthExpired => "NSAC_AUTH_EXPIRED",
            Self::NsacUnavailable => "NSAC_UNAVAILABLE",
            Self::UserNotLinked => "USER_NOT_LINKED",
            Self::InvalidParams => "INVALID_PARAMS",
            Self::SessionRefreshError => "SESSION_REFRESH_ERROR",
            Self::CryptoError => "CRYPTO_ERROR",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::NotFound => "NOT_FOUND",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }

    pub const fn http_status(&self) -> u16 {
        match self {
            Self::InvalidNsacCredentials | Self::NsacAuthExpired | Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::UserNotLinked | Self::NotFound => 404,
            Self::InvalidParams => 400,
            Self::SessionRefreshError => 502,
            Self::NsacUnavailable => 503,
            Self::CryptoError | Self::InternalError => 500,
        }
    }

    pub const fn default_user_message(&self) -> &'static str {
        match self {
            Self::InvalidNsacCredentials => "E-mail ou senha do NSAC inválidos.",
            Self::NsacAuthExpired => "Sua sessão do NSAC expirou. Faça login novamente.",
            Self::NsacUnavailable => {
                "O portal NSAC está temporariamente indisponível. Tente novamente em instantes."
            }
            Self::UserNotLinked => "Nenhuma conta NSAC vinculada a este usuário.",
            Self::InvalidParams => "Parâmetros de consulta inválidos.",
            Self::SessionRefreshError => "Não foi possível renovar a sessão do NSAC.",
            Self::CryptoError => "Erro interno ao processar dados protegidos.",
            Self::Unauthorized => "Autenticação necessária para acessar este recurso.",
            Self::Forbidden => "Você não tem permissão para acessar este recurso.",
            Self::NotFound => "Recurso não encontrado.",
            Self::InternalError => "Ocorreu um erro interno no servidor.",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub struct AppError {
    pub code: ErrorCode,
    pub user_message: String,
    internal_message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl AppError {
    pub fn new(code: ErrorCode, internal_message: impl Into<String>) -> Self {
        Self {
            code,
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
            user_message: user_message.into(),
            internal_message: internal_message.into(),
            source: None,
        }
    }

    pub fn with_source<E>(
        code: ErrorCode,
        internal_message: impl Into<String>,
        source: E,
    ) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self {
            code,
            user_message: code.default_user_message().to_string(),
            internal_message: internal_message.into(),
            source: Some(Box::new(source)),
        }
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

    pub fn user_not_linked(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UserNotLinked, internal_message)
    }

    pub fn invalid_params(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, internal_message)
    }

    pub fn session_refresh(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::SessionRefreshError, internal_message)
    }

    pub fn crypto(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::CryptoError, internal_message)
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

    pub fn internal(internal_message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InternalError, internal_message)
    }

    pub const fn http_status(&self) -> u16 {
        self.code.http_status()
    }

    pub fn internal_message(&self) -> &str {
        &self.internal_message
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.user_message)
    }
}

impl fmt::Debug for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppError")
            .field("code", &self.code)
            .field("user_message", &self.user_message)
            .field("internal_message", &self.internal_message)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("AppError", 2)?;
        state.serialize_field("code", &self.code)?;
        state.serialize_field("message", &self.user_message)?;
        state.end()
    }
}
