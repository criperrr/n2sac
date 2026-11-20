"""Tier 3 — Pairwise cross-feature combinations (10 tests)."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.assertions import (
    bimester_class,
    bimester_personal,
    class_average_grade,
    iter_boletim_cells,
    personal_grade,
    personal_recovery,
)
from harness.http_client import assert_error_envelope, assert_success_envelope, get_field
from harness.mock_nsac import MockAtrasoRecord, MockFichaEntry

from tests._helpers import (
    OpaqueE2ETestCase,
    assert_boletim_envelope,
    assert_etag_header,
    find_bimester,
    find_subject,
    find_year,
)


class TestTier3Combinations(OpaqueE2ETestCase):
    def test_t3_pair_01_service_discord_filters_and_post_filter_etag_304(self) -> None:
        _, _, account_id, _, _, _ = self.create_and_login_noturno(
            "t3_pair_01", class_code="INF1"
        )
        discord_id = self.ctx.unique_discord_id("950")
        svc = self.ctx.service_client()
        assert_success_envelope(svc.put_discord_link(account_id, discord_id))

        r1 = svc.get_boletim(
            discord_id=discord_id,
            filters={"classAverage[gt]": "7.0", "subjectName[contains]": "matematica"},
        )
        p1, years_1 = assert_boletim_envelope(r1, require_poll_hint=True)
        self.assertEqual(len(years_1), 1)
        cells_1 = list(iter_boletim_cells(p1))
        self.assertGreater(len(cells_1), 0)
        for _, subj, _, bim in cells_1:
            self.assertEqual(subj, "Matemática")
            avg = class_average_grade(bimester_class(bim))
            self.assertIsNotNone(avg)
            assert avg is not None
            self.assertGreater(avg, 7.0)
        etag_1 = assert_etag_header(r1)

        r_304 = svc.get_boletim(
            discord_id=discord_id,
            filters={"classAverage[gt]": "7.0", "subjectName[contains]": "matematica"},
            if_none_match=etag_1,
        )
        self.assertEqual(r_304.status, 304)
        self.assertEqual(r_304.raw_body, b"")

        r_b2 = svc.get_boletim(
            discord_id=discord_id,
            filters={"targetBimester": "2"},
            if_none_match=etag_1,
        )
        p_b2, _ = assert_boletim_envelope(r_b2)
        self.assertTrue(all(b == 2 for _, _, b, _ in iter_boletim_cells(p_b2)))
        etag_2 = assert_etag_header(r_b2)
        self.assertNotEqual(etag_1, etag_2)

    def test_t3_pair_02_login_sentinel_fanout_fcm_cache_invalidation_and_etag(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        cohort = self.login_class_cohort("INF2", 2, tag_prefix="t3_pair_02")
        r1 = cohort[0].client.get_boletim()
        assert_boletim_envelope(r1)
        etag_1 = assert_etag_header(r1)
        assert_boletim_envelope(cohort[1].client.get_boletim())

        self.ctx.mock_nsac.release_class_grade(
            "INF2", "Física", 3, mt_grade="7.9", mb_grade="8.8"
        )
        self.ctx.db.trigger_sentinel_due_now("INF2")

        for member in cohort:
            self.ctx.mock_fcm.wait_for_messages(
                1, timeout=10.0, token=member.fcm_token, only_succeeded=True
            )

        self.ctx.wait_until(
            lambda: cohort[0].client.get_boletim(if_none_match=etag_1).status == 200,
            timeout=10.0,
            description="sentinel invalidates moka cache and rotates ETag after fan-out",
        )

        r2 = cohort[0].client.get_boletim(if_none_match=etag_1)
        _, years_2 = assert_boletim_envelope(r2)
        etag_2 = assert_etag_header(r2)
        self.assertNotEqual(etag_1, etag_2)
        fis = find_subject(find_year(years_2, 2026), "Física")
        self.assertEqual(personal_grade(bimester_personal(find_bimester(fis, 3))), 8.8)

    def test_t3_pair_03_fcm_token_rotation_delivers_only_to_new_token(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        tok_v1 = self.unique_fcm_token("t3_pair_03_v1")
        tok_v2 = self.unique_fcm_token("t3_pair_03_v2")
        _, _, _, _, client, _ = self.create_and_login_noturno(
            "t3_pair_03", class_code="INF3", fcm_token=tok_v1
        )
        assert_success_envelope(client.put_fcm_token(tok_v2))

        self.ctx.mock_nsac.release_class_grade(
            "INF3", "Química", 3, mt_grade="8.0", mb_grade="8.6"
        )
        self.ctx.db.trigger_sentinel_due_now("INF3")

        self.ctx.mock_fcm.wait_for_messages(
            1, timeout=10.0, token=tok_v2, only_succeeded=True
        )
        self.assertEqual(self.ctx.mock_fcm.count_messages(token=tok_v1), 0)

    def test_t3_pair_04_service_discord_link_bearer_verify_and_filtered_read(self) -> None:
        _, _, account_id, _, client, _ = self.create_and_login_diurno(
            "t3_pair_04", class_code="INI3A"
        )
        discord_id = self.ctx.unique_discord_id("951")
        svc = self.ctx.service_client()

        assert_success_envelope(svc.put_discord_link(account_id, discord_id))

        acct_data = assert_success_envelope(client.get_account())
        self.assertEqual(get_field(acct_data, "discord_id", "discordId"), discord_id)

        rec_resp = svc.get_boletim(
            discord_id=discord_id,
            filters={"isRecovery": "true"},
        )
        p_rec, _ = assert_boletim_envelope(rec_resp)
        cells = list(iter_boletim_cells(p_rec))
        self.assertGreater(len(cells), 0)
        for _, _, _, bim in cells:
            self.assertTrue(personal_recovery(bimester_personal(bim)))

    def test_t3_pair_05_stale_fcm_404_cleanup_preserves_sibling_delivery(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        tok_dead = self.unique_fcm_token("t3_pair_05_dead")
        tok_live = self.unique_fcm_token("t3_pair_05_live")
        email_a, _, _, _, client_a, _ = self.create_and_login_noturno(
            "t3_pair_05_a", class_code="ELE1", fcm_token=tok_dead
        )
        self.create_and_login_noturno(
            "t3_pair_05_b", class_code="ELE1", fcm_token=tok_live
        )

        self.ctx.mock_fcm.set_token_error(tok_dead, status=404, error_code="UNREGISTERED")
        self.ctx.mock_nsac.release_class_grade(
            "ELE1", "História", 3, mt_grade="8.1", mb_grade="8.7"
        )
        self.ctx.db.trigger_sentinel_due_now("ELE1")

        self.ctx.mock_fcm.wait_for_messages(
            1, timeout=10.0, token=tok_live, only_succeeded=True
        )
        self.ctx.mock_fcm.wait_for_messages(
            1, timeout=10.0, token=tok_dead, only_succeeded=False
        )
        self.ctx.wait_until(
            lambda: (self.ctx.db.get_account_row(email_a) or {}).get("fcm_token") is None,
            timeout=8.0,
            description="dead fcm_token cleared while sibling received push",
        )

        acct_a = assert_success_envelope(client_a.get_account())
        has_push = get_field(
            acct_a,
            "has_fcm_token",
            "hasFcmToken",
            "push_enabled",
            "pushEnabled",
            "fcm_registered",
            "fcmRegistered",
        )
        if has_push is not None:
            self.assertFalse(bool(has_push))

    def test_t3_pair_06_dynamic_ficha_mutation_updates_payload_and_etag(self) -> None:
        self.require_managed_server()
        assert self.ctx.mock_nsac is not None

        email, _, _, _, client, student = self.create_and_login_noturno(
            "t3_pair_06", class_code="ELE2"
        )
        r_null = client.get_ficha_detail(3)
        self.assertIsNone(assert_success_envelope(r_null))
        etag_null = assert_etag_header(r_null)

        updated_fichas = dict(student.fichas)
        updated_fichas[3] = MockFichaEntry(
            bimester=3,
            last_viewed="19/11/2026 09:00",
            available=True,
            status_text="Visualizar",
            detail_rows=[("Química", "Atenção", "Revisar estequiometria.")],
            detail_notes=["Atendimento quinta-feira."],
        )
        self.ctx.mock_nsac.set_fichas(email, updated_fichas)

        # Restart without truncating DB or resetting mocks to drop in-memory moka cache.
        self.ctx.restart_server(truncate=False, reset_mocks=False)

        r_avail = client.get_ficha_detail(3)
        detail = assert_success_envelope(r_avail)
        self.assertIsInstance(detail, dict)
        self.assertEqual(detail["bimester"], 3)
        self.assertIn("Química", repr(detail.get("tables", [])))
        etag_avail = assert_etag_header(r_avail)
        self.assertNotEqual(etag_null, etag_avail)

    def test_t3_pair_07_alternating_cookie_order_and_auto_refresh_across_routes(self) -> None:
        self.require_managed_mocks()
        self.require_db()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.db is not None

        self.ctx.mock_nsac.set_cookie_order("alternating")
        email, _, _, _, client, _ = self.create_and_login_noturno(
            "t3_pair_07", class_code="ELE3"
        )
        self.ctx.mock_nsac.expire_student_sessions(email)

        assert_success_envelope(client.get_home())
        assert_success_envelope(client.get_atrasos())
        assert_success_envelope(client.get_saidas())
        assert_boletim_envelope(client.get_boletim())
        assert_success_envelope(client.get_ficha_detail(2))

        row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(row)
        assert row is not None
        self.assertTrue(str(row["nsac_cookies_enc"]).startswith("v1:"))

    def test_t3_pair_08_db_persistence_across_server_restart_under_nsac_503(self) -> None:
        self.require_managed_server()
        assert self.ctx.mock_nsac is not None

        email, _, _, _, client, _ = self.create_and_login_noturno(
            "t3_pair_08", class_code="MEC1"
        )
        self.ctx.mock_nsac.set_atrasos(
            email, [MockAtrasoRecord("20/05/2026", "07:15", "Chuva forte")]
        )
        r_bol = client.get_boletim()
        assert_boletim_envelope(r_bol)
        etag_bol = assert_etag_header(r_bol)
        atrasos_1 = assert_success_envelope(client.get_atrasos())
        self.assertEqual(len(atrasos_1), 1)

        self.ctx.restart_server(truncate=False, reset_mocks=False)
        self.ctx.mock_nsac.set_maintenance("503")

        r_bol_304 = client.get_boletim(if_none_match=etag_bol)
        self.assertEqual(r_bol_304.status, 304)

        r_bol_200 = client.get_boletim()
        _, years = assert_boletim_envelope(r_bol_200)
        self.assertEqual(len(years), 1)

        atrasos_2 = assert_success_envelope(client.get_atrasos())
        self.assertEqual(len(atrasos_2), 1)
        self.assertEqual(atrasos_2[0]["reason"], "Chuva forte")

    def test_t3_pair_09_dual_service_auth_modes_share_etag_304(self) -> None:
        _, _, account_id, _, _, _ = self.create_and_login_noturno(
            "t3_pair_09", class_code="MEC2"
        )

        svc_header = self.ctx.service_client(mode="header")
        svc_bearer = self.ctx.service_client(mode="bearer_service")

        r1 = svc_header.get_boletim(account_id=account_id)
        assert_boletim_envelope(r1)
        etag = assert_etag_header(r1)

        r2 = svc_bearer.get_boletim(account_id=account_id, if_none_match=etag)
        self.assertEqual(r2.status, 304)
        self.assertEqual(r2.raw_body, b"")

    def test_t3_pair_10_logout_revokes_bearer_without_stopping_sentinel_or_service(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        tok = self.unique_fcm_token("t3_pair_10")
        email, _, account_id, _, client, _ = self.create_and_login_noturno(
            "t3_pair_10", class_code="MEC3", fcm_token=tok
        )
        assert_success_envelope(client.logout())
        assert_error_envelope(client.get_boletim(), 401, {"UNAUTHORIZED"})

        row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(row)
        assert row is not None
        self.assertTrue(row["active"])

        svc_resp = self.ctx.service_client().get_boletim(account_id=account_id)
        assert_boletim_envelope(svc_resp)

        self.ctx.mock_nsac.release_class_grade(
            "MEC3", "Matemática", 3, mt_grade="8.4", mb_grade="9.0"
        )
        self.ctx.db.trigger_sentinel_due_now("MEC3")
        self.ctx.mock_fcm.wait_for_messages(
            1, timeout=10.0, token=tok, only_succeeded=True
        )


if __name__ == "__main__":
    unittest.main()
