"""Shared BaseE2ETestCase and opaque-box BoletimData assertion helpers."""

from __future__ import annotations

import atexit
from typing import Any, Iterator
import unittest

from harness.context import E2EContext, get_context, teardown_if_auto_created
from harness.http_client import (
    ApiClient,
    assert_success_envelope,
    get_field,
)
from harness.mock_nsac import MockStudent

atexit.register(teardown_if_auto_created)


def boletim_years(payload: dict[str, Any]) -> list[dict[str, Any]]:
    years = payload.get("data")
    if isinstance(years, list):
        return years
    raise AssertionError(f"Expected 'data' list in BoletimData, got: {payload!r}")


def year_number(year_obj: dict[str, Any]) -> int:
    val = get_field(year_obj, "year", "schoolYear", "school_year")
    if not isinstance(val, int):
        raise AssertionError(f"Missing integer year in {year_obj!r}")
    return val


def year_title(year_obj: dict[str, Any]) -> str:
    return str(get_field(year_obj, "title", "classCode", "class_code", default=""))


def year_status(year_obj: dict[str, Any]) -> str:
    return str(get_field(year_obj, "status", default=""))


def year_grades(year_obj: dict[str, Any]) -> list[dict[str, Any]]:
    grades = get_field(year_obj, "grades", "subjects", default=[])
    return list(grades or [])


def year_metrics(year_obj: dict[str, Any]) -> list[dict[str, Any]]:
    metrics = get_field(
        year_obj, "bimestersMetrics", "bimesters_metrics", "bimesters", default=[]
    )
    return list(metrics or [])


def subject_name(subj_obj: dict[str, Any]) -> str:
    return str(
        get_field(subj_obj, "subjectName", "subject_name", "name", default="")
    )


def subject_bimesters(subj_obj: dict[str, Any]) -> list[dict[str, Any]]:
    return list(get_field(subj_obj, "bimesters", default=[]) or [])


def subject_results(subj_obj: dict[str, Any]) -> dict[str, Any]:
    return dict(get_field(subj_obj, "results", "finalResult", "final_result", default={}) or {})


def bimester_number(bim_obj: dict[str, Any]) -> int:
    val = get_field(bim_obj, "bimester", "bimesterNumber", "bimester_number")
    if not isinstance(val, int):
        raise AssertionError(f"Missing bimester number in {bim_obj!r}")
    return val


def bimester_personal(bim_obj: dict[str, Any]) -> dict[str, Any]:
    return dict(get_field(bim_obj, "personal", default={}) or {})


def bimester_class(bim_obj: dict[str, Any]) -> dict[str, Any]:
    return dict(get_field(bim_obj, "class", "classInfo", "class_info", default={}) or {})


def personal_grade(pers_obj: dict[str, Any]) -> float | None:
    return get_field(pers_obj, "grade")


def personal_absences(pers_obj: dict[str, Any]) -> int:
    return int(get_field(pers_obj, "absences", default=0) or 0)


def personal_recovery(pers_obj: dict[str, Any]) -> bool:
    return bool(get_field(pers_obj, "recovery", "isRecovery", "is_recovery", default=False))


def personal_recovery_code(pers_obj: dict[str, Any]) -> str:
    return str(
        get_field(pers_obj, "recoveryCode", "recovery_code", "statusRec", "status_rec", default="")
    )


def personal_recovery_message(pers_obj: dict[str, Any]) -> str:
    return str(
        get_field(
            pers_obj,
            "recoveryMessage",
            "recovery_message",
            "recMessage",
            "rec_message",
            default="",
        )
    )


def class_average_grade(cls_obj: dict[str, Any]) -> float | None:
    return get_field(cls_obj, "averageGrade", "average_grade", "classAverage", "class_average")


def iter_boletim_cells(
    payload: dict[str, Any],
) -> Iterator[tuple[int, str, int, dict[str, Any]]]:
    for yr in boletim_years(payload):
        y_num = year_number(yr)
        for subj in year_grades(yr):
            s_name = subject_name(subj)
            for bim in subject_bimesters(subj):
                yield y_num, s_name, bimester_number(bim), bim


class BaseE2ETestCase(unittest.TestCase):
    """Base test case providing shared harness access and student factories."""

    ctx: E2EContext

    def setUp(self) -> None:
        self.ctx = get_context()

    def require_managed_mocks(self) -> None:
        if not self.ctx.managed_mocks or self.ctx.mock_nsac is None or self.ctx.mock_fcm is None:
            self.skipTest("Requires managed MockNsacServer/MockFcmServer")

    def require_managed_server(self) -> None:
        self.require_managed_mocks()
        if not self.ctx.managed_server:
            self.skipTest("Requires managed nsac-server process")

    def require_db(self) -> None:
        if self.ctx.db is None:
            self.skipTest("PostgreSQL DbHelper unavailable")

    def create_and_login_noturno(
        self,
        tag: str,
        *,
        class_code: str = "INF1",
        password: str = "PassTest_Noturno_01!",
        unposted_final: bool = True,
        lowercase_nac_tooltip: bool = False,
        omit_remember_cookie: bool = False,
        discord_id: str | None = None,
        fcm_token: str | None = None,
        via_service: bool = False,
    ) -> tuple[str, str, int, str | None, ApiClient, MockStudent]:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None
        email = self.ctx.unique_email(tag)
        student = self.ctx.mock_nsac.create_noturno_student(
            email,
            password,
            class_code=class_code,
            unposted_final=unposted_final,
            lowercase_nac_tooltip=lowercase_nac_tooltip,
            omit_remember_cookie=omit_remember_cookie,
        )
        client = (
            self.ctx.service_client() if via_service else self.ctx.api_client()
        )
        resp = client.login(
            email, password, discord_id=discord_id, fcm_token=fcm_token
        )
        data = assert_success_envelope(resp)
        account_id = int(data["account_id"])
        token = data.get("token")
        user_client = (
            self.ctx.api_client(bearer_token=token) if token else self.ctx.api_client()
        )
        return email, password, account_id, token, user_client, student

    def create_and_login_diurno(
        self,
        tag: str,
        *,
        class_code: str = "INI3A",
        password: str = "PassTest_Diurno_01!",
        lowercase_nac_tooltip: bool = False,
        include_unposted_dash_bim2: bool = False,
        omit_remember_cookie: bool = False,
        discord_id: str | None = None,
        fcm_token: str | None = None,
        via_service: bool = False,
    ) -> tuple[str, str, int, str | None, ApiClient, MockStudent]:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None
        email = self.ctx.unique_email(tag)
        student = self.ctx.mock_nsac.create_diurno_student(
            email,
            password,
            class_code=class_code,
            lowercase_nac_tooltip=lowercase_nac_tooltip,
            include_unposted_dash_bim2=include_unposted_dash_bim2,
            omit_remember_cookie=omit_remember_cookie,
        )
        client = (
            self.ctx.service_client() if via_service else self.ctx.api_client()
        )
        resp = client.login(
            email, password, discord_id=discord_id, fcm_token=fcm_token
        )
        data = assert_success_envelope(resp)
        account_id = int(data["account_id"])
        token = data.get("token")
        user_client = (
            self.ctx.api_client(bearer_token=token) if token else self.ctx.api_client()
        )
        return email, password, account_id, token, user_client, student
