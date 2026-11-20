"""Shared E2E runtime context coordinating mocks, DB, and nsac-server."""

from __future__ import annotations

import os
import time
from typing import Callable

from harness.config import E2EConfig
from harness.db import DbHelper
from harness.http_client import ApiClient
from harness.mock_fcm import MockFcmServer
from harness.mock_nsac import MockNsacServer
from harness.server_proc import ServerProcess, find_free_port, resolve_server_binary


class E2EContext:
    """Lifecycle container shared across Tier 1-4 unittest modules."""

    def __init__(
        self,
        config: E2EConfig,
        *,
        mock_nsac: MockNsacServer | None,
        mock_fcm: MockFcmServer | None,
        db: DbHelper | None,
        server: ServerProcess | None,
        base_url: str,
        managed_mocks: bool,
        managed_server: bool,
        owns_db: bool,
    ) -> None:
        self.config = config
        self.mock_nsac = mock_nsac
        self.mock_fcm = mock_fcm
        self.db = db
        self.server = server
        self.base_url = base_url.rstrip("/")
        self.managed_mocks = managed_mocks
        self.managed_server = managed_server
        self.owns_db = owns_db
        self._counter = 0

    def next_seq(self) -> int:
        self._counter += 1
        return self._counter

    def unique_email(self, tag: str) -> str:
        seq = self.next_seq()
        safe = "".join(ch if ch.isalnum() or ch == "_" else "_" for ch in tag.lower())
        return f"e2e_{safe}_{seq}@nsac-e2e.invalid"

    def unique_discord_id(self, prefix: str = "900") -> str:
        seq = self.next_seq()
        return f"{prefix}{seq:012d}"

    def api_client(
        self,
        *,
        bearer_token: str | None = None,
        service_key: str | None = None,
        service_auth_mode: str = "header",
    ) -> ApiClient:
        return ApiClient(
            self.base_url,
            bearer_token=bearer_token,
            service_key=service_key,
            service_auth_mode=service_auth_mode,  # type: ignore[arg-type]
        )

    def service_client(self, mode: str = "header") -> ApiClient:
        return self.api_client(
            service_key=self.config.service_api_key,
            service_auth_mode=mode,
        )

    def wait_until(
        self,
        predicate: Callable[[], bool],
        *,
        timeout: float = 12.0,
        interval: float = 0.15,
        description: str = "condition",
    ) -> None:
        deadline = time.monotonic() + timeout
        last_exc: Exception | None = None
        while time.monotonic() < deadline:
            try:
                if predicate():
                    return
                last_exc = None
            except Exception as exc:
                last_exc = exc
            time.sleep(interval)
        extra = f" (last error: {last_exc})" if last_exc else ""
        raise TimeoutError(f"Timed out after {timeout:.1f}s waiting for {description}{extra}")

    def restart_server(
        self,
        *,
        truncate: bool = True,
        seed_fn: Callable[[DbHelper], None] | None = None,
        reset_mocks: bool = True,
    ) -> None:
        if not self.managed_server or self.server is None:
            raise RuntimeError("Cannot restart externally attached nsac-server")
        assert self.db is not None
        assert self.mock_nsac is not None
        assert self.mock_fcm is not None

        env_vars = dict(self.server.env_vars)
        bin_path = self.server.binary_path
        http_bind = self.server.http_bind

        self.server.stop()
        self.server.close_logs()

        if reset_mocks:
            self.mock_nsac.reset()
            self.mock_fcm.reset()

        if truncate:
            self.db.truncate_runtime_tables(preserve_service_clients=True)

        if seed_fn is not None:
            seed_fn(self.db)

        new_proc = ServerProcess(
            binary_path=bin_path,
            env_vars=env_vars,
            http_bind=http_bind,
            cwd=self.config.repo_root,
        ).start()
        self.server = new_proc
        self.base_url = new_proc.base_url
        self.db.seed_service_client(
            self.config.service_client_name,
            self.config.service_api_key,
        )

    def teardown(self) -> None:
        if self.server is not None:
            self.server.stop()
            self.server.close_logs()
            self.server = None
        if self.mock_nsac is not None:
            self.mock_nsac.stop()
            self.mock_nsac = None
        if self.mock_fcm is not None:
            self.mock_fcm.stop()
            self.mock_fcm = None
        if self.owns_db and self.db is not None and not self.config.keep_db:
            try:
                self.db.drop_database()
            except Exception:
                pass


_ACTIVE_CONTEXT: E2EContext | None = None
_CONTEXT_OWNER: bool = False


def set_active_context(ctx: E2EContext | None) -> None:
    global _ACTIVE_CONTEXT, _CONTEXT_OWNER
    _ACTIVE_CONTEXT = ctx
    _CONTEXT_OWNER = False


def get_context() -> E2EContext:
    global _ACTIVE_CONTEXT, _CONTEXT_OWNER
    if _ACTIVE_CONTEXT is not None:
        return _ACTIVE_CONTEXT
    # Fallback bootstrap if unittest is invoked directly without run_e2e.py
    cfg = E2EConfig.from_env()
    _ACTIVE_CONTEXT = bootstrap_context(cfg)
    _CONTEXT_OWNER = True
    return _ACTIVE_CONTEXT


def teardown_if_auto_created() -> None:
    global _ACTIVE_CONTEXT, _CONTEXT_OWNER
    if _CONTEXT_OWNER and _ACTIVE_CONTEXT is not None:
        _ACTIVE_CONTEXT.teardown()
        _ACTIVE_CONTEXT = None
        _CONTEXT_OWNER = False


def bootstrap_context(config: E2EConfig) -> E2EContext:
    db = DbHelper(config.db_url, config.admin_db_url)
    if config.base_url:
        # Attached mode: external server already running
        db_ok = db.check_connectivity()
        return E2EContext(
            config=config,
            mock_nsac=None,
            mock_fcm=None,
            db=db if db_ok else None,
            server=None,
            base_url=config.base_url,
            managed_mocks=False,
            managed_server=False,
            owns_db=False,
        )

    binary = resolve_server_binary(
        config.repo_root, config.server_bin, no_build=config.no_build
    )
    mock_nsac = MockNsacServer().start()
    mock_fcm = MockFcmServer().start()
    try:
        db.reset_database()
        api_port = find_free_port()
        http_bind = f"127.0.0.1:{api_port}"
        base_env = os.environ.copy()
        base_env.update(
            config.to_server_env(
                http_bind=http_bind,
                mock_nsac_base_url=mock_nsac.base_url,
                mock_fcm_base_url=mock_fcm.base_url,
            )
        )
        server = ServerProcess(
            binary_path=binary,
            env_vars=base_env,
            http_bind=http_bind,
            cwd=config.repo_root,
        ).start()
        db.seed_service_client(
            config.service_client_name,
            config.service_api_key,
        )
        return E2EContext(
            config=config,
            mock_nsac=mock_nsac,
            mock_fcm=mock_fcm,
            db=db,
            server=server,
            base_url=server.base_url,
            managed_mocks=True,
            managed_server=True,
            owns_db=True,
        )
    except Exception:
        mock_nsac.stop()
        mock_fcm.stop()
        raise
