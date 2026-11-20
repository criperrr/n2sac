"""Shared opaque-box E2E test helpers, isolation lifecycle, and domain extractors."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import sys
from typing import Any, Callable

_E2E_ROOT = str(Path(__file__).resolve().parents[1])
if _E2E_ROOT not in sys.path:
    sys.path.insert(0, _E2E_ROOT)

from harness.assertions import (  # noqa: E402
    BaseE2ETestCase,
    bimester_number,
    boletim_years,
    subject_bimesters,
    subject_name,
    year_grades,
    year_number,
)
from harness.db import DbHelper  # noqa: E402
from harness.http_client import (  # noqa: E402
    ApiClient,
    ApiResponse,
    assert_success_envelope,
)
from harness.mock_nsac import MockStudent  # noqa: E402


@dataclass
class CohortMember:
    email: str
    password: str
    account_id: int
    token: str | None
    client: ApiClient
    student: MockStudent
    fcm_token: str | None
    discord_id: str | None


class OpaqueE2ETestCase(BaseE2ETestCase):
    """Base test case adding automatic mock cleanup and clean-slate isolation helpers."""

    def setUp(self) -> None:
        super().setUp()
        if self.ctx.mock_nsac is not None:
            self.addCleanup(self.ctx.mock_nsac.set_maintenance, "off")
            self.addCleanup(self.ctx.mock_nsac.set_cookie_order, "normal")
            self.addCleanup(self.ctx.mock_nsac.clear_requests)
        if self.ctx.mock_fcm is not None:
            self.addCleanup(self.ctx.mock_fcm.clear_global_error)
            self.addCleanup(self.ctx.mock_fcm.clear_token_error)
            self.addCleanup(self.ctx.mock_fcm.set_require_bearer, True)

    def reset_isolated_state(
        self,
        *,
        seed_fn: Callable[[DbHelper], None] | None = None,
        truncate: bool = True,
        reset_mocks: bool = True,
    ) -> None:
        self.require_managed_server()
        self.ctx.restart_server(
            truncate=truncate,
            seed_fn=seed_fn,
            reset_mocks=reset_mocks,
        )

    def unique_fcm_token(self, tag: str) -> str:
        seq = self.ctx.next_seq()
        safe = "".join(ch if ch.isalnum() or ch == "_" else "_" for ch in tag.lower())
        return f"fcm_e2e_{safe}_{seq}"

    def login_class_cohort(
        self,
        class_code: str,
        count: int,
        *,
        tag_prefix: str = "cohort",
        diurno: bool = False,
        with_fcm: bool = True,
        with_discord: bool = False,
    ) -> list[CohortMember]:
        members: list[CohortMember] = []
        for idx in range(count):
            tag = f"{tag_prefix}_{class_code.lower()}_{idx}"
            fcm_tok = self.unique_fcm_token(tag) if with_fcm else None
            disc_id = self.ctx.unique_discord_id() if with_discord else None
            if diurno:
                email, password, account_id, token, client, student = (
                    self.create_and_login_diurno(
                        tag,
                        class_code=class_code,
                        fcm_token=fcm_tok,
                        discord_id=disc_id,
                    )
                )
            else:
                email, password, account_id, token, client, student = (
                    self.create_and_login_noturno(
                        tag,
                        class_code=class_code,
                        fcm_token=fcm_tok,
                        discord_id=disc_id,
                    )
                )
            members.append(
                CohortMember(
                    email=email,
                    password=password,
                    account_id=account_id,
                    token=token,
                    client=client,
                    student=student,
                    fcm_token=fcm_tok,
                    discord_id=disc_id,
                )
            )
        return members


def assert_boletim_envelope(
    resp: ApiResponse,
    *,
    require_poll_hint: bool = False,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    """Validate envelope and return `(boletim_payload_for_extractors, years_list)`."""
    data = assert_success_envelope(resp, require_poll_hint=require_poll_hint)
    if isinstance(data, list):
        if not isinstance(resp.json_body, dict):
            raise AssertionError(f"Expected dict envelope, got: {resp.json_body!r}")
        return resp.json_body, boletim_years(resp.json_body)
    if isinstance(data, dict) and isinstance(data.get("data"), list):
        return data, boletim_years(data)
    raise AssertionError(
        f"Expected boletim list or AllYearsResponse object, got: {resp.json_body!r}"
    )


def assert_etag_header(resp: ApiResponse) -> str:
    etag = resp.header("ETag")
    if not etag or not etag.strip():
        raise AssertionError(f"Missing ETag header in response: {resp.headers!r}")
    return etag.strip()


def find_year(years: list[dict[str, Any]], target_year: int) -> dict[str, Any]:
    for yr in years:
        if year_number(yr) == target_year:
            return yr
    raise AssertionError(f"Year {target_year} not found in {[year_number(y) for y in years]}")


def find_subject(year_obj: dict[str, Any], target_name: str) -> dict[str, Any]:
    for subj in year_grades(year_obj):
        if subject_name(subj) == target_name:
            return subj
    available = [subject_name(s) for s in year_grades(year_obj)]
    raise AssertionError(f"Subject {target_name!r} not found in {available}")


def find_bimester(subj_obj: dict[str, Any], target_bim: int) -> dict[str, Any]:
    for bim in subject_bimesters(subj_obj):
        if bimester_number(bim) == target_bim:
            return bim
    available = [bimester_number(b) for b in subject_bimesters(subj_obj)]
    raise AssertionError(f"Bimester {target_bim} not found in {available}")


def count_boletim_cells(years: list[dict[str, Any]]) -> int:
    total = 0
    for yr in years:
        for subj in year_grades(yr):
            total += len(subject_bimesters(subj))
    return total
