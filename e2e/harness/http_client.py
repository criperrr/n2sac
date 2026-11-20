"""Stdlib HTTP API client and opaque-box envelope/tolerance helpers."""

from __future__ import annotations

from dataclasses import dataclass
import json
from typing import Any, Literal
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import HTTPErrorProcessor, OpenerDirector, Request, build_opener


class _NoErrorRaiseProcessor(HTTPErrorProcessor):
    """Allow 304/4xx/5xx responses to return normally with headers and body intact."""

    def http_response(self, request: Request, response: Any) -> Any:
        return response

    https_response = http_response


@dataclass
class ApiResponse:
    status: int
    headers: dict[str, str]
    raw_body: bytes
    text: str
    json_body: Any | None

    def header(self, name: str) -> str | None:
        target = name.lower()
        for k, v in self.headers.items():
            if k.lower() == target:
                return v
        return None


def get_field(obj: dict[str, Any], *aliases: str, default: Any = None) -> Any:
    """Opaque-box helper tolerating camelCase vs snake_case aliases (T-AMB-02)."""
    for key in aliases:
        if key in obj:
            return obj[key]
    return default


def encode_query_pairs(pairs: list[tuple[str, str]] | dict[str, str] | None) -> str:
    if not pairs:
        return ""
    return urlencode(pairs, doseq=True)


class ApiClient:
    """HTTP client targeting nsac-server's /api/v1 endpoints."""

    def __init__(
        self,
        base_url: str,
        *,
        bearer_token: str | None = None,
        service_key: str | None = None,
        service_auth_mode: Literal["header", "bearer_service"] = "header",
        timeout: float = 15.0,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.bearer_token = bearer_token
        self.service_key = service_key
        self.service_auth_mode = service_auth_mode
        self.timeout = timeout
        self._opener: OpenerDirector = build_opener(_NoErrorRaiseProcessor())

    def with_bearer(self, token: str | None) -> "ApiClient":
        return ApiClient(
            self.base_url,
            bearer_token=token,
            service_key=None,
            timeout=self.timeout,
        )

    def with_service_key(
        self,
        key: str,
        mode: Literal["header", "bearer_service"] = "header",
    ) -> "ApiClient":
        return ApiClient(
            self.base_url,
            bearer_token=None,
            service_key=key,
            service_auth_mode=mode,
            timeout=self.timeout,
        )

    def request(
        self,
        method: str,
        path: str,
        *,
        query: dict[str, str] | list[tuple[str, str]] | None = None,
        json_body: Any | None = None,
        raw_body: bytes | None = None,
        content_type: str | None = None,
        headers: dict[str, str] | None = None,
        if_none_match: str | None = None,
        timeout: float | None = None,
    ) -> ApiResponse:
        url = f"{self.base_url}{path}"
        qs = encode_query_pairs(query)
        if qs:
            sep = "&" if "?" in url else "?"
            url = f"{url}{sep}{qs}"

        req_headers: dict[str, str] = {"Accept": "application/json"}
        if self.bearer_token is not None:
            req_headers["Authorization"] = f"Bearer {self.bearer_token}"
        if self.service_key is not None:
            if self.service_auth_mode == "bearer_service":
                if "Authorization" not in req_headers:
                    req_headers["Authorization"] = f"Service {self.service_key}"
                else:
                    req_headers["X-Service-Key"] = self.service_key
            else:
                req_headers["X-Service-Key"] = self.service_key
        if if_none_match is not None:
            req_headers["If-None-Match"] = if_none_match
        if headers:
            req_headers.update(headers)

        data: bytes | None = None
        if raw_body is not None:
            data = raw_body
            if content_type is not None:
                req_headers["Content-Type"] = content_type
        elif json_body is not None:
            data = json.dumps(json_body).encode("utf-8")
            req_headers.setdefault("Content-Type", "application/json")

        req = Request(url, data=data, headers=req_headers, method=method.upper())
        try:
            resp = self._opener.open(req, timeout=timeout or self.timeout)
            status = int(resp.status)
            resp_headers = {k: v for k, v in resp.headers.items()}
            body_bytes = resp.read()
        except HTTPError as err:
            status = int(err.code)
            resp_headers = {k: v for k, v in (err.headers.items() if err.headers else [])}
            body_bytes = err.read()

        text = body_bytes.decode("utf-8", errors="replace")
        parsed_json: Any | None = None
        if body_bytes:
            try:
                parsed_json = json.loads(text)
            except ValueError:
                parsed_json = None

        return ApiResponse(
            status=status,
            headers=resp_headers,
            raw_body=body_bytes,
            text=text,
            json_body=parsed_json,
        )

    def get(
        self,
        path: str,
        *,
        query: dict[str, str] | list[tuple[str, str]] | None = None,
        if_none_match: str | None = None,
        headers: dict[str, str] | None = None,
        timeout: float | None = None,
    ) -> ApiResponse:
        return self.request(
            "GET",
            path,
            query=query,
            if_none_match=if_none_match,
            headers=headers,
            timeout=timeout,
        )

    def post(
        self,
        path: str,
        *,
        json_body: Any | None = None,
        raw_body: bytes | None = None,
        content_type: str | None = None,
        query: dict[str, str] | list[tuple[str, str]] | None = None,
        headers: dict[str, str] | None = None,
        timeout: float | None = None,
    ) -> ApiResponse:
        return self.request(
            "POST",
            path,
            query=query,
            json_body=json_body,
            raw_body=raw_body,
            content_type=content_type,
            headers=headers,
            timeout=timeout,
        )

    def put(
        self,
        path: str,
        *,
        json_body: Any | None = None,
        query: dict[str, str] | list[tuple[str, str]] | None = None,
        headers: dict[str, str] | None = None,
        timeout: float | None = None,
    ) -> ApiResponse:
        return self.request(
            "PUT",
            path,
            query=query,
            json_body=json_body,
            headers=headers,
            timeout=timeout,
        )

    # ---- Domain convenience endpoints ----

    def health(self) -> ApiResponse:
        return self.get("/api/v1/health")

    def login(
        self,
        nsac_email: str,
        nsac_password: str,
        *,
        discord_id: str | None = None,
        fcm_token: str | None = None,
    ) -> ApiResponse:
        payload: dict[str, Any] = {
            "nsac_email": nsac_email,
            "nsac_password": nsac_password,
        }
        if discord_id is not None:
            payload["discord_id"] = discord_id
        if fcm_token is not None:
            payload["fcm_token"] = fcm_token
        return self.post("/api/v1/auth/login", json_body=payload)

    def logout(self) -> ApiResponse:
        return self.post("/api/v1/auth/logout")

    def get_account(
        self,
        *,
        account_id: int | None = None,
        discord_id: str | None = None,
    ) -> ApiResponse:
        query: dict[str, str] = {}
        if account_id is not None:
            query["account_id"] = str(account_id)
        if discord_id is not None:
            query["discord_id"] = discord_id
        return self.get("/api/v1/account", query=query or None)

    def put_fcm_token(
        self,
        fcm_token: str,
        *,
        account_id: int | None = None,
        discord_id: str | None = None,
    ) -> ApiResponse:
        query: dict[str, str] = {}
        if account_id is not None:
            query["account_id"] = str(account_id)
        if discord_id is not None:
            query["discord_id"] = discord_id
        return self.put(
            "/api/v1/account/fcm-token",
            json_body={"fcm_token": fcm_token},
            query=query or None,
        )

    def put_discord_link(self, account_id: int, discord_id: str) -> ApiResponse:
        return self.put(
            "/api/v1/account/discord-link",
            json_body={"account_id": account_id, "discord_id": discord_id},
        )

    def _target_query(
        self,
        *,
        account_id: int | None = None,
        discord_id: str | None = None,
        extra: dict[str, str] | list[tuple[str, str]] | None = None,
    ) -> list[tuple[str, str]] | None:
        pairs: list[tuple[str, str]] = []
        if account_id is not None:
            pairs.append(("account_id", str(account_id)))
        if discord_id is not None:
            pairs.append(("discord_id", discord_id))
        if isinstance(extra, dict):
            pairs.extend(extra.items())
        elif extra is not None:
            pairs.extend(extra)
        return pairs or None

    def get_home(
        self,
        *,
        account_id: int | None = None,
        discord_id: str | None = None,
        if_none_match: str | None = None,
    ) -> ApiResponse:
        return self.get(
            "/api/v1/nsac/home",
            query=self._target_query(account_id=account_id, discord_id=discord_id),
            if_none_match=if_none_match,
        )

    def get_boletim(
        self,
        *,
        account_id: int | None = None,
        discord_id: str | None = None,
        filters: dict[str, str] | list[tuple[str, str]] | None = None,
        if_none_match: str | None = None,
    ) -> ApiResponse:
        return self.get(
            "/api/v1/nsac/boletim",
            query=self._target_query(
                account_id=account_id, discord_id=discord_id, extra=filters
            ),
            if_none_match=if_none_match,
        )

    def get_atrasos(
        self,
        *,
        account_id: int | None = None,
        discord_id: str | None = None,
        if_none_match: str | None = None,
    ) -> ApiResponse:
        return self.get(
            "/api/v1/nsac/atrasos",
            query=self._target_query(account_id=account_id, discord_id=discord_id),
            if_none_match=if_none_match,
        )

    def get_saidas(
        self,
        *,
        account_id: int | None = None,
        discord_id: str | None = None,
        if_none_match: str | None = None,
    ) -> ApiResponse:
        return self.get(
            "/api/v1/nsac/saidas",
            query=self._target_query(account_id=account_id, discord_id=discord_id),
            if_none_match=if_none_match,
        )

    def get_ficha_list(
        self,
        *,
        account_id: int | None = None,
        discord_id: str | None = None,
        if_none_match: str | None = None,
    ) -> ApiResponse:
        return self.get(
            "/api/v1/nsac/ficha-avaliacao",
            query=self._target_query(account_id=account_id, discord_id=discord_id),
            if_none_match=if_none_match,
        )

    def get_ficha_detail(
        self,
        bimester_id: int | str,
        *,
        account_id: int | None = None,
        discord_id: str | None = None,
        if_none_match: str | None = None,
        timeout: float | None = None,
    ) -> ApiResponse:
        return self.get(
            f"/api/v1/nsac/ficha-avaliacao/{bimester_id}",
            query=self._target_query(account_id=account_id, discord_id=discord_id),
            if_none_match=if_none_match,
            timeout=timeout,
        )


# ---- Opaque-box assertion helpers ----


def assert_success_envelope(
    resp: ApiResponse,
    *,
    require_poll_hint: bool = False,
) -> Any:
    if resp.status != 200:
        raise AssertionError(
            f"Expected HTTP 200, got {resp.status}: {resp.text}"
        )
    if not isinstance(resp.json_body, dict):
        raise AssertionError(f"Expected JSON object body, got: {resp.text}")
    if resp.json_body.get("success") is not True:
        raise AssertionError(f"Expected success=true, got: {resp.json_body}")
    if "data" not in resp.json_body:
        raise AssertionError(f"Missing 'data' key in envelope: {resp.json_body}")
    if require_poll_hint:
        hint = resp.json_body.get("next_poll_hint_secs")
        if not isinstance(hint, int) or isinstance(hint, bool) or hint < 0:
            raise AssertionError(
                f"Expected non-negative integer next_poll_hint_secs, got {hint!r}"
            )
    return resp.json_body["data"]


def assert_error_envelope(
    resp: ApiResponse,
    expected_status: int | tuple[int, ...],
    expected_codes: set[str] | None = None,
) -> list[dict[str, Any]]:
    statuses = (
        (expected_status,)
        if isinstance(expected_status, int)
        else expected_status
    )
    if resp.status not in statuses:
        raise AssertionError(
            f"Expected HTTP status in {statuses}, got {resp.status}: {resp.text}"
        )
    if not isinstance(resp.json_body, dict):
        raise AssertionError(f"Expected JSON error envelope, got: {resp.text}")
    if resp.json_body.get("success") is not False:
        raise AssertionError(f"Expected success=false, got: {resp.json_body}")
    errors = resp.json_body.get("errors")
    if not isinstance(errors, list) or not errors:
        raise AssertionError(f"Expected non-empty 'errors' list, got: {resp.json_body}")
    for err in errors:
        if not isinstance(err, dict):
            raise AssertionError(f"Error entry must be object: {err!r}")
        if not isinstance(err.get("code"), str) or not err["code"]:
            raise AssertionError(f"Missing error code in {err!r}")
        if not isinstance(err.get("message"), str) or not err["message"].strip():
            raise AssertionError(f"Missing pt-BR error message in {err!r}")
    if expected_codes is not None:
        actual_codes = {e["code"] for e in errors}
        if not actual_codes.intersection(expected_codes):
            raise AssertionError(
                f"Expected error code in {sorted(expected_codes)}, got {sorted(actual_codes)}"
            )
    return errors


def assert_no_secrets_leaked(
    resp: ApiResponse,
    forbidden_substrings: list[str],
) -> None:
    haystacks = [resp.text] + [f"{k}: {v}" for k, v in resp.headers.items()]
    combined = "\n".join(haystacks)
    for secret in forbidden_substrings:
        if secret and secret in combined:
            raise AssertionError(
                f"Secret substring leaked in HTTP response: {secret!r}"
            )
