"""Tier 2 — Area D: Boletim query filter boundaries and validation errors."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.assertions import (
    bimester_number,
    bimester_personal,
    iter_boletim_cells,
    personal_grade,
)
from harness.http_client import assert_error_envelope

from tests._helpers import OpaqueE2ETestCase, assert_boletim_envelope


class TestTier2Filters(OpaqueE2ETestCase):
    def test_t2_filt_01_zero_match_filters_return_200_empty_years(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_filt_01", class_code="INF1")

        for filters in ({"grade[gt]": "10.0"}, {"schoolYear": "1999"}):
            resp = client.get_boletim(filters=filters)
            _, years = assert_boletim_envelope(resp)
            self.assertEqual(years, [])

    def test_t2_filt_02_unknown_field_or_unsupported_operator_returns_400(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_filt_02", class_code="INF1")

        for filters in (
            {"unknownField": "1"},
            {"grade[contains]": "8"},
            {"subjectName[gt]": "a"},
        ):
            resp = client.get_boletim(filters=filters)
            assert_error_envelope(resp, 400, {"INVALID_PARAMS"})

    def test_t2_filt_03_malformed_bracket_syntax_returns_400(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_filt_03", class_code="INF1")

        for raw_pair in (
            [("grade[gt", "8")],
            [("grade[]", "8")],
            [("grade[[gt]]", "8")],
        ):
            resp = client.get_boletim(filters=raw_pair)
            assert_error_envelope(resp, 400, {"INVALID_PARAMS"})

    def test_t2_filt_04_empty_or_blank_list_items_return_400(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_filt_04", class_code="INF1")

        for filters in (
            {"subjectName": ""},
            {"subjectName": "Mat,,Fis"},
            {"grade": ""},
        ):
            resp = client.get_boletim(filters=filters)
            assert_error_envelope(resp, 400, {"INVALID_PARAMS"})

    def test_t2_filt_05_non_numeric_non_finite_and_invalid_bool_return_400(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_filt_05", class_code="INF1")

        for filters in (
            {"grade": "abc"},
            {"grade": "NaN"},
            {"grade": "inf"},
            {"isRecovery": "maybe"},
        ):
            resp = client.get_boletim(filters=filters)
            assert_error_envelope(resp, 400, {"INVALID_PARAMS"})

    def test_t2_filt_06_pf_extra_02_numeric_filters_exclude_none_but_match_zero(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_diurno(
            "t2_filt_06", class_code="INI3A", include_unposted_dash_bim2=True
        )

        # In 2026 Matematica: B1=8.5, B2=None, B3=None, B4=0.0
        r_lt = client.get_boletim(
            filters={"schoolYear": "2026", "subjectName[eq]": "Matemática", "grade[lt]": "5.0"}
        )
        p_lt, _ = assert_boletim_envelope(r_lt)
        lt_bims = [
            (bimester_number(b), personal_grade(bimester_personal(b)))
            for _, _, _, b in iter_boletim_cells(p_lt)
        ]
        self.assertEqual(lt_bims, [(4, 0.0)])

        r_eq = client.get_boletim(
            filters={"schoolYear": "2026", "subjectName[eq]": "Matemática", "grade[eq]": "0.0"}
        )
        p_eq, _ = assert_boletim_envelope(r_eq)
        eq_bims = [
            (bimester_number(b), personal_grade(bimester_personal(b)))
            for _, _, _, b in iter_boletim_cells(p_eq)
        ]
        self.assertEqual(eq_bims, [(4, 0.0)])

    def test_t2_filt_07_out_of_range_bimester_and_contradictory_booleans_return_empty(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_filt_07", class_code="INF1")

        for filters in (
            {"targetBimester": "0"},
            {"targetBimester": "5"},
            {"isRecovery[eq]": "false", "isRecovery[neq]": "false"},
        ):
            resp = client.get_boletim(filters=filters)
            _, years = assert_boletim_envelope(resp)
            self.assertEqual(years, [])


if __name__ == "__main__":
    unittest.main()
