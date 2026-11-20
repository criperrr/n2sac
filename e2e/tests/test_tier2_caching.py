"""Tier 2 — Area E: ETag, If-None-Match, and cache invalidation boundaries."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.assertions import bimester_personal, personal_grade
from harness.http_client import assert_success_envelope

from tests._helpers import (
    OpaqueE2ETestCase,
    assert_boletim_envelope,
    assert_etag_header,
    find_bimester,
    find_subject,
    find_year,
)


class TestTier2Caching(OpaqueE2ETestCase):
    def test_t2_cache_01_stale_if_none_match_returns_200_with_fresh_etag(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_cache_01", class_code="INF1")

        resp = client.get_boletim(if_none_match='"stale-nonexistent-etag-000"')
        _, years = assert_boletim_envelope(resp)
        self.assertEqual(len(years), 1)
        etag = assert_etag_header(resp)
        self.assertNotEqual(etag, '"stale-nonexistent-etag-000"')

    def test_t2_cache_02_multi_value_and_wildcard_if_none_match_return_304(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_cache_02", class_code="INF2")

        r1 = client.get_boletim()
        assert_boletim_envelope(r1)
        etag = assert_etag_header(r1)

        r_multi = client.get_boletim(if_none_match=f'"old-etag-1", {etag}')
        self.assertEqual(r_multi.status, 304)
        self.assertEqual(r_multi.raw_body, b"")
        self.assertEqual(assert_etag_header(r_multi), etag)

        r_wild = client.get_boletim(if_none_match="*")
        self.assertEqual(r_wild.status, 304)
        self.assertEqual(r_wild.raw_body, b"")
        self.assertEqual(assert_etag_header(r_wild), etag)

    def test_t2_cache_03_cross_filter_etag_mismatch_returns_200(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_cache_03", class_code="INF3")

        r1 = client.get_boletim(filters={"targetBimester": "1"})
        assert_boletim_envelope(r1)
        etag_b1 = assert_etag_header(r1)

        r2 = client.get_boletim(
            filters={"targetBimester": "2"},
            if_none_match=etag_b1,
        )
        _, years_b2 = assert_boletim_envelope(r2)
        self.assertEqual(len(years_b2), 1)
        etag_b2 = assert_etag_header(r2)
        self.assertNotEqual(etag_b1, etag_b2)

    def test_t2_cache_04_etag_rotates_after_sentinel_detects_grade_release(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.db is not None

        _, _, _, _, client, _ = self.create_and_login_noturno("t2_cache_04", class_code="MEI3")
        r1 = client.get_boletim()
        assert_boletim_envelope(r1)
        etag_1 = assert_etag_header(r1)

        self.ctx.mock_nsac.release_class_grade(
            "MEI3", "Física", 3, mt_grade="8.6", mb_grade="9.2"
        )
        self.ctx.db.trigger_sentinel_due_now("MEI3")

        def updated_available() -> bool:
            resp = client.get_boletim(if_none_match=etag_1)
            return resp.status == 200

        self.ctx.wait_until(
            updated_available,
            timeout=10.0,
            description="sentinel invalidates cache and rotates boletim ETag",
        )

        r2 = client.get_boletim(if_none_match=etag_1)
        _, years_2 = assert_boletim_envelope(r2)
        etag_2 = assert_etag_header(r2)
        self.assertNotEqual(etag_1, etag_2)
        fis = find_subject(find_year(years_2, 2026), "Física")
        b3_grade = personal_grade(bimester_personal(find_bimester(fis, 3)))
        self.assertEqual(b3_grade, 9.2)

    def test_t2_cache_05_null_ficha_detail_supports_etag_and_304(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t2_cache_05", class_code="ELE1")

        r1 = client.get_ficha_detail(1)
        self.assertIsNone(assert_success_envelope(r1))
        etag = assert_etag_header(r1)

        r2 = client.get_ficha_detail(1, if_none_match=etag)
        self.assertEqual(r2.status, 304)
        self.assertEqual(r2.raw_body, b"")
        self.assertEqual(assert_etag_header(r2), etag)


if __name__ == "__main__":
    unittest.main()
