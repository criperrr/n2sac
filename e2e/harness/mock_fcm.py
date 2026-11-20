"""In-process Mock FCM HTTP v1 & OAuth2 server for E2E push verification."""

from __future__ import annotations

import copy
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import re
import threading
import time
from typing import Any
from urllib.parse import urlparse


_SEND_PATH_RE = re.compile(
    r"^/v1/projects/(?P<project_id>[A-Za-z0-9_-]+)/messages:send$"
)


@dataclass
class CapturedFcmMessage:
    seq: int
    timestamp: float
    project_id: str
    auth_header: str | None
    bearer_token: str | None
    token: str
    notification: dict[str, Any] | None
    data: dict[str, Any] | None
    android: dict[str, Any] | None
    raw_body: dict[str, Any]
    response_status: int
    error_code: str | None


@dataclass
class _ErrorRule:
    status: int
    error_code: str
    grpc_status: str
    message: str
    remaining: int | None = None


class _MockFcmHTTPServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class MockFcmServer:
    """Thread-safe Google FCM HTTP v1 + OAuth2 mock server."""

    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._cond = threading.Condition(self._lock)
        self._httpd: _MockFcmHTTPServer | None = None
        self._thread: threading.Thread | None = None
        self._seq: int = 0
        self._messages: list[CapturedFcmMessage] = []
        self._token_errors: dict[str, _ErrorRule] = {}
        self._global_error: _ErrorRule | None = None
        self._require_bearer: bool = True

    @property
    def port(self) -> int:
        if self._httpd is None:
            raise RuntimeError("MockFcmServer is not started")
        return int(self._httpd.server_address[1])

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def send_url(self, project_id: str = "nsac-e2e-test") -> str:
        return f"{self.base_url}/v1/projects/{project_id}/messages:send"

    @property
    def oauth_token_url(self) -> str:
        return f"{self.base_url}/oauth2/token"

    def start(self) -> "MockFcmServer":
        if self._httpd is not None:
            return self
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: object) -> None:  # noqa: A002
                return

            def do_POST(self) -> None:  # noqa: N802
                owner._handle_post(self)

        self._httpd = _MockFcmHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._httpd.serve_forever, daemon=True)
        self._thread.start()
        return self

    def stop(self) -> None:
        if self._httpd is not None:
            self._httpd.shutdown()
            self._httpd.server_close()
            self._httpd = None
        if self._thread is not None:
            self._thread.join(timeout=3.0)
            self._thread = None

    def __enter__(self) -> "MockFcmServer":
        return self.start()

    def __exit__(self, *_: object) -> None:
        self.stop()

    def reset(self) -> None:
        with self._cond:
            self._seq = 0
            self._messages.clear()
            self._token_errors.clear()
            self._global_error = None
            self._require_bearer = True
            self._cond.notify_all()

    def set_require_bearer(self, required: bool) -> None:
        with self._cond:
            self._require_bearer = required

    def set_token_error(
        self,
        fcm_token: str,
        status: int = 404,
        error_code: str = "UNREGISTERED",
        grpc_status: str = "NOT_FOUND",
        message: str = "Requested entity was not found.",
        remaining: int | None = None,
    ) -> None:
        with self._cond:
            self._token_errors[fcm_token] = _ErrorRule(
                status=status,
                error_code=error_code,
                grpc_status=grpc_status,
                message=message,
                remaining=remaining,
            )

    def clear_token_error(self, fcm_token: str | None = None) -> None:
        with self._cond:
            if fcm_token is None:
                self._token_errors.clear()
            else:
                self._token_errors.pop(fcm_token, None)

    def set_global_error(
        self,
        status: int = 503,
        error_code: str = "UNAVAILABLE",
        grpc_status: str = "UNAVAILABLE",
        message: str = "The service is currently unavailable.",
        remaining: int | None = None,
    ) -> None:
        with self._cond:
            self._global_error = _ErrorRule(
                status=status,
                error_code=error_code,
                grpc_status=grpc_status,
                message=message,
                remaining=remaining,
            )

    def clear_global_error(self) -> None:
        with self._cond:
            self._global_error = None

    def get_messages(
        self,
        token: str | None = None,
        only_succeeded: bool = False,
    ) -> list[CapturedFcmMessage]:
        with self._lock:
            return self._filter_messages_locked(token=token, only_succeeded=only_succeeded)

    def count_messages(
        self,
        token: str | None = None,
        only_succeeded: bool = False,
    ) -> int:
        with self._lock:
            return len(
                self._filter_messages_locked(token=token, only_succeeded=only_succeeded)
            )

    def wait_for_messages(
        self,
        min_count: int,
        timeout: float = 10.0,
        token: str | None = None,
        only_succeeded: bool = False,
    ) -> list[CapturedFcmMessage]:
        with self._cond:
            ok = self._cond.wait_for(
                lambda: len(
                    self._filter_messages_locked(
                        token=token, only_succeeded=only_succeeded
                    )
                )
                >= min_count,
                timeout=timeout,
            )
            if not ok:
                current = len(
                    self._filter_messages_locked(
                        token=token, only_succeeded=only_succeeded
                    )
                )
                raise TimeoutError(
                    f"Timed out waiting for >= {min_count} FCM message(s); got {current}"
                )
            return self._filter_messages_locked(token=token, only_succeeded=only_succeeded)

    def _filter_messages_locked(
        self,
        token: str | None = None,
        only_succeeded: bool = False,
    ) -> list[CapturedFcmMessage]:
        result: list[CapturedFcmMessage] = []
        for msg in self._messages:
            if token is not None and msg.token != token:
                continue
            if only_succeeded and msg.response_status != 200:
                continue
            result.append(copy.deepcopy(msg))
        return result

    def _send_json(
        self,
        handler: BaseHTTPRequestHandler,
        status: int,
        payload: dict[str, Any],
    ) -> None:
        raw = json.dumps(payload).encode("utf-8")
        handler.send_response(status)
        handler.send_header("Content-Type", "application/json; charset=utf-8")
        handler.send_header("Content-Length", str(len(raw)))
        handler.end_headers()
        handler.wfile.write(raw)

    def _format_fcm_error(self, rule: _ErrorRule) -> dict[str, Any]:
        return {
            "error": {
                "code": rule.status,
                "message": rule.message,
                "status": rule.grpc_status,
                "details": [
                    {
                        "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                        "errorCode": rule.error_code,
                    }
                ],
            }
        }

    def _handle_post(self, handler: BaseHTTPRequestHandler) -> None:
        path = urlparse(handler.path).path
        length = int(handler.headers.get("Content-Length", "0") or "0")
        raw_body = handler.rfile.read(length)

        if path == "/oauth2/token":
            self._send_json(
                handler,
                200,
                {
                    "access_token": "mock-fcm-oauth2-access-token",
                    "expires_in": 3600,
                    "token_type": "Bearer",
                },
            )
            return

        match = _SEND_PATH_RE.match(path)
        if not match:
            self._send_json(
                handler,
                404,
                {
                    "error": {
                        "code": 404,
                        "message": f"Unknown path: {path}",
                        "status": "NOT_FOUND",
                    }
                },
            )
            return

        project_id = match.group("project_id")
        auth_header = handler.headers.get("Authorization")
        bearer_token: str | None = None
        if auth_header and auth_header.startswith("Bearer "):
            bearer_token = auth_header[len("Bearer "):].strip() or None

        with self._cond:
            require_bearer = self._require_bearer

        if require_bearer and not bearer_token:
            self._send_json(
                handler,
                401,
                {
                    "error": {
                        "code": 401,
                        "message": "Request is missing required authentication credential.",
                        "status": "UNAUTHENTICATED",
                    }
                },
            )
            return

        try:
            payload = json.loads(raw_body.decode("utf-8"))
        except Exception:
            self._send_json(
                handler,
                400,
                self._format_fcm_error(
                    _ErrorRule(
                        status=400,
                        error_code="INVALID_ARGUMENT",
                        grpc_status="INVALID_ARGUMENT",
                        message="Malformed JSON body",
                    )
                ),
            )
            return

        message = payload.get("message") if isinstance(payload, dict) else None
        token = message.get("token") if isinstance(message, dict) else None
        if not isinstance(message, dict) or not isinstance(token, str) or not token.strip():
            self._send_json(
                handler,
                400,
                self._format_fcm_error(
                    _ErrorRule(
                        status=400,
                        error_code="INVALID_ARGUMENT",
                        grpc_status="INVALID_ARGUMENT",
                        message="Missing message.token",
                    )
                ),
            )
            return

        with self._cond:
            selected_rule: _ErrorRule | None = None
            if self._global_error is not None:
                rule = self._global_error
                selected_rule = copy.copy(rule)
                if rule.remaining is not None:
                    rule.remaining -= 1
                    if rule.remaining <= 0:
                        self._global_error = None
            elif token in self._token_errors:
                rule = self._token_errors[token]
                selected_rule = copy.copy(rule)
                if rule.remaining is not None:
                    rule.remaining -= 1
                    if rule.remaining <= 0:
                        self._token_errors.pop(token, None)

            self._seq += 1
            seq = self._seq
            status = selected_rule.status if selected_rule else 200
            err_code = selected_rule.error_code if selected_rule else None

            self._messages.append(
                CapturedFcmMessage(
                    seq=seq,
                    timestamp=time.time(),
                    project_id=project_id,
                    auth_header=auth_header,
                    bearer_token=bearer_token,
                    token=token,
                    notification=copy.deepcopy(message.get("notification")),
                    data=copy.deepcopy(message.get("data")),
                    android=copy.deepcopy(message.get("android")),
                    raw_body=copy.deepcopy(payload),
                    response_status=status,
                    error_code=err_code,
                )
            )
            self._cond.notify_all()

        if selected_rule is not None:
            self._send_json(handler, selected_rule.status, self._format_fcm_error(selected_rule))
            return

        self._send_json(
            handler,
            200,
            {"name": f"projects/{project_id}/messages/mock-msg-{seq}"},
        )
