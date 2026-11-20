use std::fmt;
use std::net::SocketAddr;

use nsac_core::crypto::CryptoService;
use url::Url;
use zeroize::Zeroizing;

use crate::error::ConfigError;

#[derive(Clone)]
pub struct ServiceKeySeed {
    pub name: String,
    pub raw_key: Zeroizing<String>,
    pub scopes: Vec<String>,
}

impl fmt::Debug for ServiceKeySeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceKeySeed")
            .field("name", &self.name)
            .field("raw_key", &"[REDACTED]")
            .field("scopes", &self.scopes)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct SentinelConfig {
    pub enabled: bool,
    pub tick_ms: u64,
    pub cold_baseline_secs: u64,
    pub historical_radius_days: u16,
    pub historical_interval_secs: u64,
    pub hot_min_interval_secs: u64,
    pub hot_max_interval_secs: u64,
    pub hot_duration_secs: u64,
    pub api_volume_weight: f64,
    pub api_cooldown_secs: u64,
    pub midnight_backoff_secs: u64,
    pub jitter_max_ms: u64,
}

#[derive(Clone)]
pub struct FcmConfig {
    pub enabled: bool,
    pub project_id: Option<String>,
    pub credentials_json: Option<Zeroizing<String>>,
    pub base_url: Option<String>,
    pub endpoint_url: Option<String>,
    pub oauth_token_url: Option<String>,
}

impl fmt::Debug for FcmConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FcmConfig")
            .field("enabled", &self.enabled)
            .field("project_id", &self.project_id)
            .field(
                "credentials_json",
                &self.credentials_json.as_ref().map(|_| "[REDACTED]"),
            )
            .field("base_url", &self.base_url)
            .field("endpoint_url", &self.endpoint_url)
            .field("oauth_token_url", &self.oauth_token_url)
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CacheConfig {
    pub boletim_ttl_secs: u64,
    pub snapshot_ttl_secs: u64,
    pub auth_ttl_secs: u64,
}

#[derive(Clone)]
pub struct Config {
    pub http_bind: SocketAddr,
    pub rust_log: String,
    pub database_url: Zeroizing<String>,
    pub encryption_key: Zeroizing<String>,
    pub service_api_keys: Vec<ServiceKeySeed>,
    pub user_session_ttl_secs: u64,
    pub nsac_base_url: String,
    pub nsac_timeout_secs: u64,
    pub nsac_max_concurrent_requests: usize,
    pub sentinel: SentinelConfig,
    pub fcm: FcmConfig,
    pub cache: CacheConfig,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let client_names: Vec<&str> = self
            .service_api_keys
            .iter()
            .map(|k| k.name.as_str())
            .collect();
        f.debug_struct("Config")
            .field("http_bind", &self.http_bind)
            .field("rust_log", &self.rust_log)
            .field("database_url", &redact_database_url(&self.database_url))
            .field("encryption_key", &"[REDACTED]")
            .field("service_api_key_clients", &client_names)
            .field("user_session_ttl_secs", &self.user_session_ttl_secs)
            .field("nsac_base_url", &self.nsac_base_url)
            .field("nsac_timeout_secs", &self.nsac_timeout_secs)
            .field(
                "nsac_max_concurrent_requests",
                &self.nsac_max_concurrent_requests,
            )
            .field("sentinel", &self.sentinel)
            .field("fcm", &self.fcm)
            .field("cache", &self.cache)
            .finish()
    }
}

fn redact_database_url(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(mut parsed) => {
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some("REDACTED"));
            }
            if !parsed.username().is_empty() {
                let _ = parsed.set_username("REDACTED");
            }
            parsed.to_string()
        }
        Err(_) => "[REDACTED]".to_string(),
    }
}

fn non_empty<F>(lookup: &F, names: &[&'static str]) -> Option<(&'static str, String)>
where
    F: Fn(&str) -> Option<String>,
{
    for &name in names {
        if let Some(raw) = lookup(name) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Some((name, trimmed.to_string()));
            }
        }
    }
    None
}

fn parse_bool(name: &'static str, raw: &str) -> Result<bool, ConfigError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::InvalidVar {
            name,
            reason: "expected boolean (true/false/1/0)",
        }),
    }
}

fn parse_u64_min<F>(
    lookup: &F,
    names: &[&'static str],
    default: u64,
    min: u64,
) -> Result<u64, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    match non_empty(lookup, names) {
        Some((name, val)) => {
            let parsed = val.parse::<u64>().map_err(|_| ConfigError::InvalidVar {
                name,
                reason: "expected non-negative integer",
            })?;
            if parsed < min {
                return Err(ConfigError::InvalidVar {
                    name,
                    reason: "value is below minimum allowed threshold",
                });
            }
            Ok(parsed)
        }
        None => Ok(default),
    }
}

fn parse_usize_min<F>(
    lookup: &F,
    names: &[&'static str],
    default: usize,
    min: usize,
) -> Result<usize, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    match non_empty(lookup, names) {
        Some((name, val)) => {
            let parsed = val.parse::<usize>().map_err(|_| ConfigError::InvalidVar {
                name,
                reason: "expected positive integer",
            })?;
            if parsed < min {
                return Err(ConfigError::InvalidVar {
                    name,
                    reason: "value is below minimum allowed threshold",
                });
            }
            Ok(parsed)
        }
        None => Ok(default),
    }
}

fn parse_service_api_keys(raw: Option<String>) -> Result<Vec<ServiceKeySeed>, ConfigError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };

    let default_scopes = vec![
        "nsac:read".to_string(),
        "nsac:write".to_string(),
        "account:link".to_string(),
    ];

    let mut seeds = Vec::new();
    for (idx, item) in raw.split(',').enumerate() {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((name, key)) = trimmed.split_once(':') {
            let name = name.trim();
            let key = key.trim();
            if name.is_empty() || key.is_empty() {
                return Err(ConfigError::InvalidVar {
                    name: "SERVICE_API_KEYS",
                    reason: "name:key entry cannot have empty name or secret key",
                });
            }
            seeds.push(ServiceKeySeed {
                name: name.to_string(),
                raw_key: Zeroizing::new(key.to_string()),
                scopes: default_scopes.clone(),
            });
        } else {
            seeds.push(ServiceKeySeed {
                name: format!("service-{}", idx + 1),
                raw_key: Zeroizing::new(trimmed.to_string()),
                scopes: default_scopes.clone(),
            });
        }
    }

    Ok(seeds)
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let _ = dotenvy::dotenv();
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let http_bind = match non_empty(&lookup, &["HTTP_BIND"]) {
            Some((name, val)) => val.parse::<SocketAddr>().map_err(|_| ConfigError::InvalidVar {
                name,
                reason: "expected valid SocketAddr (e.g. 127.0.0.1:8080)",
            })?,
            None => SocketAddr::from(([127, 0, 0, 1], 8080)),
        };

        let rust_log = non_empty(&lookup, &["RUST_LOG"])
            .map(|(_, v)| v)
            .unwrap_or_else(|| "info,nsac_server=info,nsac_core=info".to_string());

        let (_, database_url_raw) = non_empty(&lookup, &["DATABASE_URL"])
            .ok_or(ConfigError::MissingVar { name: "DATABASE_URL" })?;
        let parsed_db = Url::parse(&database_url_raw).map_err(|_| ConfigError::InvalidVar {
            name: "DATABASE_URL",
            reason: "expected valid PostgreSQL connection URL",
        })?;
        if parsed_db.scheme() != "postgres" && parsed_db.scheme() != "postgresql" {
            return Err(ConfigError::InvalidVar {
                name: "DATABASE_URL",
                reason: "expected postgres:// or postgresql:// scheme",
            });
        }

        let (_, encryption_key_raw) = non_empty(&lookup, &["ENCRYPTION_KEY"]).ok_or(
            ConfigError::MissingVar {
                name: "ENCRYPTION_KEY",
            },
        )?;
        if CryptoService::from_hex_key(&encryption_key_raw).is_err() {
            return Err(ConfigError::InvalidEncryptionKey);
        }

        let service_api_keys =
            parse_service_api_keys(non_empty(&lookup, &["SERVICE_API_KEYS"]).map(|(_, v)| v))?;

        let user_session_ttl_secs = parse_u64_min(
            &lookup,
            &["USER_SESSION_TTL_SECS", "USER_TOKEN_TTL_SECS"],
            2_592_000,
            1,
        )?;

        let nsac_base_url_raw = non_empty(&lookup, &["NSAC_BASE_URL"])
            .map(|(_, v)| v.trim_end_matches('/').to_string())
            .unwrap_or_else(|| "http://200.145.153.1/nsac".to_string());
        let parsed_nsac =
            Url::parse(&nsac_base_url_raw).map_err(|_| ConfigError::InvalidVar {
                name: "NSAC_BASE_URL",
                reason: "expected valid HTTP(S) URL",
            })?;
        if parsed_nsac.scheme() != "http" && parsed_nsac.scheme() != "https" {
            return Err(ConfigError::InvalidVar {
                name: "NSAC_BASE_URL",
                reason: "expected http or https scheme",
            });
        }

        let nsac_timeout_secs = parse_u64_min(
            &lookup,
            &["NSAC_HTTP_TIMEOUT_SECS", "NSAC_TIMEOUT_SECS"],
            12,
            1,
        )?;
        let nsac_max_concurrent_requests =
            parse_usize_min(&lookup, &["NSAC_MAX_CONCURRENT_REQUESTS"], 4, 1)?;

        let sentinel_enabled = match non_empty(&lookup, &["SENTINEL_ENABLED"]) {
            Some((name, val)) => parse_bool(name, &val)?,
            None => true,
        };
        let tick_ms = parse_u64_min(&lookup, &["SENTINEL_TICK_MS"], 10_000, 1)?;
        let cold_baseline_secs = parse_u64_min(
            &lookup,
            &[
                "SENTINEL_COLD_BASELINE_SECS",
                "SENTINEL_BASELINE_INTERVAL_SECS",
            ],
            86_400,
            1,
        )?;
        let historical_radius_days_u64 =
            parse_u64_min(&lookup, &["SENTINEL_HISTORICAL_RADIUS_DAYS"], 4, 0)?;
        let historical_radius_days =
            u16::try_from(historical_radius_days_u64).map_err(|_| ConfigError::InvalidVar {
                name: "SENTINEL_HISTORICAL_RADIUS_DAYS",
                reason: "value exceeds u16 range",
            })?;
        let historical_interval_secs = parse_u64_min(
            &lookup,
            &[
                "SENTINEL_HISTORICAL_INTERVAL_SECS",
                "SENTINEL_HISTORICAL_MIN_INTERVAL_SECS",
            ],
            7_200,
            1,
        )?;

        let hot_min_env = non_empty(
            &lookup,
            &["SENTINEL_HOT_INTERVAL_SECS", "SENTINEL_HOT_MIN_INTERVAL_SECS"],
        );
        let hot_min_interval_secs = match &hot_min_env {
            Some((name, val)) => {
                let parsed = val.parse::<u64>().map_err(|_| ConfigError::InvalidVar {
                    name,
                    reason: "expected positive integer",
                })?;
                if parsed < 1 {
                    return Err(ConfigError::InvalidVar {
                        name,
                        reason: "value must be >= 1",
                    });
                }
                parsed
            }
            None => 120,
        };

        let hot_max_interval_secs =
            match non_empty(&lookup, &["SENTINEL_HOT_MAX_INTERVAL_SECS"]) {
                Some((name, val)) => {
                    let parsed = val.parse::<u64>().map_err(|_| ConfigError::InvalidVar {
                        name,
                        reason: "expected positive integer",
                    })?;
                    if parsed < 1 {
                        return Err(ConfigError::InvalidVar {
                            name,
                            reason: "value must be >= 1",
                        });
                    }
                    parsed.max(hot_min_interval_secs)
                }
                None => match hot_min_env {
                    Some(("SENTINEL_HOT_INTERVAL_SECS", _)) => hot_min_interval_secs,
                    Some(_) => 180.max(hot_min_interval_secs),
                    None => 180,
                },
            };

        let hot_duration_secs = parse_u64_min(&lookup, &["SENTINEL_HOT_DURATION_SECS"], 7_200, 1)?;

        let api_volume_weight = match non_empty(&lookup, &["SENTINEL_API_VOLUME_WEIGHT"]) {
            Some((name, val)) => {
                let parsed = val.parse::<f64>().map_err(|_| ConfigError::InvalidVar {
                    name,
                    reason: "expected floating point number in 0.0..=1.0",
                })?;
                if !parsed.is_finite() || !(0.0..=1.0).contains(&parsed) {
                    return Err(ConfigError::InvalidVar {
                        name,
                        reason: "must be finite and within 0.0..=1.0",
                    });
                }
                parsed
            }
            None => 0.15,
        };

        let api_cooldown_secs = parse_u64_min(
            &lookup,
            &[
                "SENTINEL_API_VOLUME_COOLDOWN_SECS",
                "SENTINEL_API_COOLDOWN_SECS",
            ],
            1_800,
            0,
        )?;
        let midnight_backoff_secs =
            parse_u64_min(&lookup, &["SENTINEL_MIDNIGHT_BACKOFF_SECS"], 1_800, 1)?;
        let jitter_max_ms = parse_u64_min(
            &lookup,
            &["SENTINEL_JITTER_MAX_MS", "SENTINEL_FANOUT_JITTER_MAX_MS"],
            1_500,
            0,
        )?;

        let fcm_project_id = non_empty(&lookup, &["FCM_PROJECT_ID"]).map(|(_, v)| v);
        let fcm_credentials_json =
            non_empty(&lookup, &["FCM_CREDENTIALS_JSON"]).map(|(_, v)| Zeroizing::new(v));
        let fcm_base_url =
            non_empty(&lookup, &["FCM_BASE_URL"]).map(|(_, v)| v.trim_end_matches('/').to_string());
        let fcm_endpoint_raw =
            non_empty(&lookup, &["FCM_ENDPOINT_URL", "FCM_ENDPOINT_OVERRIDE"])
                .map(|(_, v)| v);
        let fcm_oauth_raw = non_empty(&lookup, &["FCM_OAUTH_TOKEN_URL"]).map(|(_, v)| v);

        let fcm_enabled = match non_empty(&lookup, &["FCM_ENABLED"]) {
            Some((name, val)) => parse_bool(name, &val)?,
            None => {
                fcm_endpoint_raw.is_some()
                    || fcm_base_url.is_some()
                    || fcm_oauth_raw.is_some()
                    || fcm_credentials_json.is_some()
            }
        };

        let fcm_endpoint_url = if let Some(explicit) = fcm_endpoint_raw {
            Some(explicit)
        } else if let (Some(base), Some(project_id)) = (&fcm_base_url, &fcm_project_id) {
            Some(format!(
                "{base}/v1/projects/{project_id}/messages:send"
            ))
        } else if fcm_enabled {
            fcm_project_id.as_ref().map(|project_id| {
                format!("https://fcm.googleapis.com/v1/projects/{project_id}/messages:send")
            })
        } else {
            None
        };

        if fcm_enabled && fcm_endpoint_url.is_none() {
            return Err(ConfigError::MissingVar {
                name: "FCM_PROJECT_ID",
            });
        }

        let fcm_oauth_token_url = if let Some(explicit) = fcm_oauth_raw {
            Some(explicit)
        } else if let Some(base) = &fcm_base_url {
            Some(format!("{base}/oauth2/token"))
        } else if fcm_enabled {
            Some("https://oauth2.googleapis.com/token".to_string())
        } else {
            None
        };

        let boletim_ttl_secs = parse_u64_min(&lookup, &["CACHE_BOLETIM_TTL_SECS"], 300, 0)?;
        let snapshot_ttl_secs = parse_u64_min(&lookup, &["CACHE_SNAPSHOT_TTL_SECS"], 300, 0)?;
        let auth_ttl_secs = parse_u64_min(&lookup, &["CACHE_AUTH_TTL_SECS"], 0, 0)?;

        Ok(Self {
            http_bind,
            rust_log,
            database_url: Zeroizing::new(database_url_raw),
            encryption_key: Zeroizing::new(encryption_key_raw),
            service_api_keys,
            user_session_ttl_secs,
            nsac_base_url: nsac_base_url_raw,
            nsac_timeout_secs,
            nsac_max_concurrent_requests,
            sentinel: SentinelConfig {
                enabled: sentinel_enabled,
                tick_ms,
                cold_baseline_secs,
                historical_radius_days,
                historical_interval_secs,
                hot_min_interval_secs,
                hot_max_interval_secs,
                hot_duration_secs,
                api_volume_weight,
                api_cooldown_secs,
                midnight_backoff_secs,
                jitter_max_ms,
            },
            fcm: FcmConfig {
                enabled: fcm_enabled,
                project_id: fcm_project_id,
                credentials_json: fcm_credentials_json,
                base_url: fcm_base_url,
                endpoint_url: fcm_endpoint_url,
                oauth_token_url: fcm_oauth_token_url,
            },
            cache: CacheConfig {
                boletim_ttl_secs,
                snapshot_ttl_secs,
                auth_ttl_secs,
            },
        })
    }
}
