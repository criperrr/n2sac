"""Runtime configuration for the opaque-box E2E test suite."""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
from urllib.parse import urlparse, urlunparse


DEFAULT_DB_URL = "postgres://postgres@127.0.0.1:5432/nsac_e2e"
DEFAULT_ENCRYPTION_KEY = (
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
)
DEFAULT_SERVICE_CLIENT_NAME = "e2e-bot"
DEFAULT_SERVICE_API_KEY = "e2e-service-secret-key-01"
DEFAULT_FCM_PROJECT_ID = "nsac-e2e-test"


def find_repo_root() -> Path:
    return Path(__file__).resolve().parent.parent.parent


@dataclass
class E2EConfig:
    repo_root: Path
    db_url: str = DEFAULT_DB_URL
    encryption_key: str = DEFAULT_ENCRYPTION_KEY
    service_client_name: str = DEFAULT_SERVICE_CLIENT_NAME
    service_api_key: str = DEFAULT_SERVICE_API_KEY
    fcm_project_id: str = DEFAULT_FCM_PROJECT_ID
    nsac_http_timeout_secs: int = 3
    sentinel_tick_ms: int = 250
    sentinel_cold_baseline_secs: int = 30
    sentinel_historical_interval_secs: int = 2
    sentinel_hot_interval_secs: int = 1
    sentinel_hot_duration_secs: int = 10
    sentinel_api_volume_cooldown_secs: int = 2
    sentinel_jitter_max_ms: int = 50
    base_url: str | None = None
    server_bin: Path | None = None
    no_build: bool = False
    keep_db: bool = False
    verbose: bool = False

    @classmethod
    def from_env(cls, **overrides: object) -> "E2EConfig":
        raw_bin = overrides.get("server_bin") or os.environ.get("NSAC_SERVER_BIN")
        server_bin = Path(str(raw_bin)) if raw_bin else None
        base_url = overrides.get("base_url") or os.environ.get("NSAC_E2E_BASE_URL")
        db_url = str(overrides.get("db_url") or os.environ.get("NSAC_E2E_DB_URL") or DEFAULT_DB_URL)
        return cls(
            repo_root=find_repo_root(),
            db_url=db_url,
            base_url=str(base_url) if base_url else None,
            server_bin=server_bin,
            no_build=bool(overrides.get("no_build", False)),
            keep_db=bool(overrides.get("keep_db", False)),
            verbose=bool(overrides.get("verbose", False)),
        )

    @property
    def db_name(self) -> str:
        parsed = urlparse(self.db_url)
        name = parsed.path.lstrip("/")
        return name or "nsac_e2e"

    @property
    def admin_db_url(self) -> str:
        parsed = urlparse(self.db_url)
        return urlunparse(parsed._replace(path="/postgres"))

    def to_server_env(
        self,
        http_bind: str,
        mock_nsac_base_url: str,
        mock_fcm_base_url: str,
    ) -> dict[str, str]:
        fcm_base = mock_fcm_base_url.rstrip("/")
        return {
            "DATABASE_URL": self.db_url,
            "ENCRYPTION_KEY": self.encryption_key,
            "HTTP_BIND": http_bind,
            "NSAC_BASE_URL": mock_nsac_base_url.rstrip("/"),
            "NSAC_HTTP_TIMEOUT_SECS": str(self.nsac_http_timeout_secs),
            "FCM_PROJECT_ID": self.fcm_project_id,
            "FCM_BASE_URL": fcm_base,
            "FCM_ENDPOINT_URL": (
                f"{fcm_base}/v1/projects/{self.fcm_project_id}/messages:send"
            ),
            "FCM_OAUTH_TOKEN_URL": f"{fcm_base}/oauth2/token",
            "SERVICE_API_KEYS": f"{self.service_client_name}:{self.service_api_key}",
            "SENTINEL_TICK_MS": str(self.sentinel_tick_ms),
            "SENTINEL_COLD_BASELINE_SECS": str(self.sentinel_cold_baseline_secs),
            "SENTINEL_HISTORICAL_INTERVAL_SECS": str(
                self.sentinel_historical_interval_secs
            ),
            "SENTINEL_HOT_INTERVAL_SECS": str(self.sentinel_hot_interval_secs),
            "SENTINEL_HOT_DURATION_SECS": str(self.sentinel_hot_duration_secs),
            "SENTINEL_API_VOLUME_COOLDOWN_SECS": str(
                self.sentinel_api_volume_cooldown_secs
            ),
            "SENTINEL_JITTER_MAX_MS": str(self.sentinel_jitter_max_ms),
            "RUST_LOG": "info,nsac_server=debug",
        }
