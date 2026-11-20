"""Tier 1 — Area E: HTTP caching (ETag, 304 Not Modified, next_poll_hint_secs, moka)."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.http_client import assert_success_envelope
from harness.mock_nsac import MockAtrasoRecord, MockSaidaRecord

from tests._helpers import OpaqueE2ETestCase, assert_boletim_envelope, assert_etag_header


class TestTier1Caching(OpaqueE2ETestCase):
    def test_t1_cache_01_boletim_etag_poll_hint_and_304(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t1_cache_01", class_code="INF1")

        resp_1 = client.get_boletim()
        assert_boletim_envelope(resp_1, require_poll_hint=True)
        etag = assert_etag_header(resp_1)
        self.assertTrue(etag.startswith('"') and etag.endswith('"'))

        resp_2 = client.get_boletim(if_none_match=etag)
        self.assertEqual(resp_2.status, 304)
        self.assertEqual(resp_2.raw_body, b"")
        self.assertEqual(resp_2.header("ETag"), etag)

    def test_t1_cache_02_all_other_nsac_routes_support_etag_and_304(self) -> None:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None

        email, _, _, _, client, _ = self.create_and_login_noturno("t1_cache_02", class_code="INF2")
        self.ctx.mock_nsac.set_atrasos(email, [MockAtrasoRecord("10/05/2026", "07:20", "Ônibus")])
        self.ctx.mock_nsac.set_saidas(email, [MockSaidaRecord("11/05/2026", "11:10", "Médico")])

        route_calls = (
            lambda inm: client.get_home(if_none_match=inm),
            lambda inm: client.get_atrasos(if_none_match=inm),
            lambda inm: client.get_saidas(if_none_match=inm),
            lambda inm: client.get_ficha_list(if_none_match=inm),
            lambda inm: client.get_ficha_detail(2, if_none_match=inm),
        )
        for call in route_calls:
            r1 = call(None)
            assert_success_envelope(r1)
            etag = assert_etag_header(r1)
            r2 = call(etag)
            self.assertEqual(r2.status, 304)
            self.assertEqual(r2.raw_body, b"")
            self.assertEqual(r2.header("ETag"), etag)

    def test_t1_cache_03_weak_and_unquoted_if_none_match_tolerance(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t1_cache_03", class_code="INF3")

        r1 = client.get_boletim()
        assert_boletim_envelope(r1)
        etag = assert_etag_header(r1)
        unquoted = etag.strip('"')

        r_weak = client.get_boletim(if_none_match=f'W/{etag}')
        self.assertEqual(r_weak.status, 304)
        self.assertEqual(r_weak.raw_body, b"")
        self.assertEqual(assert_etag_header(r_weak), etag)

        r_unquoted = client.get_boletim(if_none_match=unquoted)
        self.assertEqual(r_unquoted.status, 304)
        self.assertEqual(r_unquoted.raw_body, b"")
        self.assertEqual(assert_etag_header(r_unquoted), etag)

    def test_t1_cache_04_post_filter_etag_stability_and_304(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t1_cache_04", class_code="ELE1")

        r_b1 = client.get_boletim(filters={"targetBimester": "1"})
        assert_boletim_envelope(r_b1, require_poll_hint=True)
        etag_b1 = assert_etag_header(r_b1)

        r_b2 = client.get_boletim(filters={"targetBimester": "2"})
        assert_boletim_envelope(r_b2, require_poll_hint=True)
        etag_b2 = assert_etag_header(r_b2)

        self.assertNotEqual(etag_b1, etag_b2)

        r_b1_304 = client.get_boletim(filters={"targetBimester": "1"}, if_none_match=etag_b1)
        self.assertEqual(r_b1_304.status, 304)
        self.assertEqual(r_b1_304.raw_body, b"")
        self.assertEqual(assert_etag_header(r_b1_304), etag_b1)

    def test_t1_cache_05_moka_cache_absorbs_repeated_boletim_reads(self) -> None:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None

        email, _, _, _, client, _ = self.create_and_login_noturno("t1_cache_05", class_code="ELE2")
        # Warm cache once after login, then clear mock request log.
        assert_boletim_envelope(client.get_boletim())
        self.ctx.mock_nsac.clear_requests()

        for _ in range(5):
            assert_boletim_envelope(client.get_boletim())

        self.assertEqual(self.ctx.mock_nsac.count_boletim_requests(student_email=email), 0)


if __name__ == "__main__":
    unittest.main()
