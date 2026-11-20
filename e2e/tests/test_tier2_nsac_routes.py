"""Tier 2 — Area C: NSAC routes & scraper edge cases (PF-04, PF-05, PF-06, PF-EXTRA-02)."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.assertions import (
    bimester_personal,
    personal_grade,
    personal_recovery,
    personal_recovery_code,
    subject_results,
)
from harness.http_client import assert_error_envelope, assert_success_envelope, get_field

from tests._helpers import (
    OpaqueE2ETestCase,
    assert_boletim_envelope,
    find_bimester,
    find_subject,
    find_year,
)


class TestTier2NsacRoutes(OpaqueE2ETestCase):
    def test_t2_nsac_01_ficha_invalid_bimester_ids_return_400(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_nsac_01", class_code="INF1")

        for bad_id in (0, 5, "abc"):
            resp = client.get_ficha_detail(bad_id)
            assert_error_envelope(resp, 400, {"INVALID_PARAMS"})

    def test_t2_nsac_02_ficha_unavailable_302_maps_to_null_data(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_nsac_02", class_code="INF2")

        for unavailable_id in (1, 3):
            resp = client.get_ficha_detail(unavailable_id)
            data = assert_success_envelope(resp)
            self.assertIsNone(data)

    def test_t2_nsac_03_pf05_unposted_final_grade_and_absences_do_not_crash(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno(
            "t2_nsac_03", class_code="INF3", unposted_final=True
        )

        resp = client.get_boletim()
        _, years = assert_boletim_envelope(resp)
        yr = find_year(years, 2026)
        mat = find_subject(yr, "Matemática")
        res = subject_results(mat)
        self.assertIsInstance(res, dict)
        self.assertTrue(
            any(k in res for k in ("grade", "finalGrade", "final_grade")),
            f"Expected final grade key in {res!r}",
        )
        self.assertIsNone(get_field(res, "grade", "finalGrade", "final_grade"))
        total_abs = get_field(res, "totalAbsences", "total_absences")
        self.assertIsNotNone(total_abs)
        self.assertEqual(total_abs, 0)

    def test_t2_nsac_04_pf_extra_02_unposted_dash_is_none_while_zero_is_zero(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_diurno(
            "t2_nsac_04", class_code="INI3A", include_unposted_dash_bim2=True
        )

        resp = client.get_boletim()
        _, years = assert_boletim_envelope(resp)
        yr_2026 = find_year(years, 2026)
        mat = find_subject(yr_2026, "Matemática")

        g1 = personal_grade(bimester_personal(find_bimester(mat, 1)))
        g2 = personal_grade(bimester_personal(find_bimester(mat, 2)))
        g3 = personal_grade(bimester_personal(find_bimester(mat, 3)))
        g4 = personal_grade(bimester_personal(find_bimester(mat, 4)))

        self.assertEqual(g1, 8.5)
        self.assertIsNone(g2)
        self.assertIsNone(g3)
        self.assertEqual(g4, 0.0)

    def test_t2_nsac_05_lowercase_nac_tooltip_parses_cleanly(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno(
            "t2_nsac_05", class_code="ELE1", lowercase_nac_tooltip=True
        )

        resp = client.get_boletim()
        _, years = assert_boletim_envelope(resp)
        yr = find_year(years, 2026)
        mat = find_subject(yr, "Matemática")
        b1_pers = bimester_personal(find_bimester(mat, 1))
        self.assertFalse(personal_recovery(b1_pers))
        self.assertEqual(personal_recovery_code(b1_pers), "NAC")

    def test_t2_nsac_06_pf06_omit_remember_cookie_two_cookie_login(self) -> None:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None

        _, _, _, _, client, _ = self.create_and_login_noturno(
            "t2_nsac_06", class_code="ELE2", omit_remember_cookie=True
        )
        resp = client.get_boletim()
        _, years = assert_boletim_envelope(resp)
        self.assertEqual(len(years), 1)

    def test_t2_nsac_07_pf06_alternating_cookie_order_across_routes(self) -> None:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None

        self.ctx.mock_nsac.set_cookie_order("alternating")
        _, _, _, _, client, _ = self.create_and_login_noturno(
            "t2_nsac_07", class_code="MEC1"
        )
        assert_success_envelope(client.get_home())
        assert_boletim_envelope(client.get_boletim())
        assert_success_envelope(client.get_atrasos())
        assert_success_envelope(client.get_saidas())

    def test_t2_nsac_08_empty_aux_lists_and_pf04_nsac_delay_timeout_503(self) -> None:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None

        _, _, _, _, client, _ = self.create_and_login_noturno(
            "t2_nsac_08", class_code="MEC2"
        )
        atrasos_data = assert_success_envelope(client.get_atrasos())
        self.assertEqual(atrasos_data, [])
        saidas_data = assert_success_envelope(client.get_saidas())
        self.assertEqual(saidas_data, [])

        # Uncached ficha detail route under NSAC delay (> 3s timeout) must fail with 503 NSAC_UNAVAILABLE.
        self.ctx.mock_nsac.set_maintenance("delay", delay_secs=4.0)
        slow_resp = client.get_ficha_detail(2, timeout=10.0)
        assert_error_envelope(slow_resp, 503, {"NSAC_UNAVAILABLE"})


if __name__ == "__main__":
    unittest.main()
