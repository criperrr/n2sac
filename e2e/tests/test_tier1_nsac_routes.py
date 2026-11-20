"""Tier 1 — Area C: All 5 mapped NSAC routes and PF-06 cookie order tolerance."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.assertions import (
    subject_bimesters,
    year_grades,
    year_metrics,
    year_number,
    year_status,
    year_title,
)
from harness.http_client import assert_success_envelope, get_field
from harness.mock_nsac import MockAtrasoRecord, MockSaidaRecord

from tests._helpers import OpaqueE2ETestCase, assert_boletim_envelope


class TestTier1NsacRoutes(OpaqueE2ETestCase):
    def test_t1_nsac_01_home_route_returns_portal_date_routes_and_links(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t1_nsac_01", class_code="INF1")

        resp = client.get_home()
        data = assert_success_envelope(resp)
        self.assertIsInstance(data, dict)

        portal_date = get_field(data, "portalDate", "portal_date")
        self.assertTrue(bool(portal_date))

        routes = get_field(data, "availableRoutes", "available_routes", default=[])
        routes_joined = " ".join(str(r) for r in routes)
        for expected in ("boletim", "atrasos", "saidas", "ficha-avaliacao"):
            self.assertIn(expected, routes_joined)

        info_links = get_field(data, "infoLinks", "info_links", default=[])
        self.assertEqual(len(info_links), 3)

    def test_t1_nsac_02_boletim_noturno_single_year_seven_subjects(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t1_nsac_02", class_code="INF1")

        resp = client.get_boletim()
        _, years = assert_boletim_envelope(resp)
        self.assertEqual(len(years), 1)

        yr = years[0]
        self.assertEqual(year_number(yr), 2026)
        self.assertEqual(year_title(yr), "INF1")
        subjects = year_grades(yr)
        self.assertEqual(len(subjects), 7)
        for subj in subjects:
            self.assertEqual(len(subject_bimesters(subj)), 4)
        self.assertEqual(len(year_metrics(yr)), 4)

    def test_t1_nsac_03_boletim_diurno_chronological_multi_year(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_diurno("t1_nsac_03", class_code="INI3A")

        resp = client.get_boletim()
        _, years = assert_boletim_envelope(resp)
        self.assertEqual(len(years), 3)

        self.assertEqual([year_number(y) for y in years], [2024, 2025, 2026])
        self.assertEqual([year_title(y) for y in years], ["INI1A", "INI2A", "INI3A"])
        self.assertEqual(year_status(years[0]), "Aprovado")
        self.assertEqual(year_status(years[1]), "Aprovado")
        self.assertIn("Cursando", year_status(years[2]))

    def test_t1_nsac_04_atrasos_route_returns_seeded_records(self) -> None:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None

        email, _, _, _, client, _ = self.create_and_login_noturno("t1_nsac_04", class_code="INF2")
        seeded = [
            MockAtrasoRecord("12/05/2026", "07:25", "Transporte público atrasado"),
            MockAtrasoRecord("19/05/2026", "19:15", "Consulta médica"),
        ]
        self.ctx.mock_nsac.set_atrasos(email, seeded)

        resp = client.get_atrasos()
        data = assert_success_envelope(resp)
        self.assertIsInstance(data, list)
        self.assertEqual(len(data), 2)
        self.assertEqual(data[0]["date"], "12/05/2026")
        self.assertEqual(data[0]["time"], "07:25")
        self.assertEqual(data[0]["reason"], "Transporte público atrasado")
        self.assertEqual(data[1]["date"], "19/05/2026")

    def test_t1_nsac_05_saidas_route_returns_seeded_records(self) -> None:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None

        email, _, _, _, client, _ = self.create_and_login_noturno("t1_nsac_05", class_code="INF3")
        seeded = [
            MockSaidaRecord("14/05/2026", "10:40", "Dispensa médica com responsável"),
        ]
        self.ctx.mock_nsac.set_saidas(email, seeded)

        resp = client.get_saidas()
        data = assert_success_envelope(resp)
        self.assertIsInstance(data, list)
        self.assertEqual(len(data), 1)
        self.assertEqual(data[0]["date"], "14/05/2026")
        self.assertEqual(data[0]["time"], "10:40")
        self.assertEqual(data[0]["reason"], "Dispensa médica com responsável")

    def test_t1_nsac_06_ficha_avaliacao_list_and_detail_ignores_title_typo(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t1_nsac_06", class_code="MEC1")

        list_resp = client.get_ficha_list()
        list_data = assert_success_envelope(list_resp)
        self.assertIsInstance(list_data, list)
        self.assertEqual(len(list_data), 3)

        by_bim = {int(item["bimester"]): item for item in list_data}
        self.assertIn(2, by_bim)
        self.assertTrue(bool(by_bim[2]["available"]))
        self.assertFalse(bool(by_bim[1]["available"]))
        self.assertFalse(bool(by_bim[3]["available"]))

        detail_resp = client.get_ficha_detail(2)
        detail = assert_success_envelope(detail_resp)
        self.assertIsInstance(detail, dict)
        self.assertEqual(detail["bimester"], 2)

        tables_text = repr(detail.get("tables", []))
        self.assertIn("Matemática", tables_text)
        self.assertIn("Física", tables_text)
        notes = detail.get("notes", [])
        self.assertTrue(any("plantão" in str(n).lower() for n in notes))

    def test_t1_nsac_07_pf06_cookie_orders_reversed_and_remember_middle(self) -> None:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None

        for mode in ("reversed", "remember_middle"):
            self.ctx.mock_nsac.set_cookie_order(mode)
            _, _, _, _, client, _ = self.create_and_login_noturno(
                f"t1_nsac_07_{mode}", class_code="MEC2"
            )
            home_resp = client.get_home()
            assert_success_envelope(home_resp)
            boletim_resp = client.get_boletim()
            _, years = assert_boletim_envelope(boletim_resp)
            self.assertEqual(len(years), 1)


if __name__ == "__main__":
    unittest.main()
