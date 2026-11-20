"""Tier 2 — Area F: Sentinel, DB isolation, and FCM push error boundaries."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.assertions import bimester_personal, personal_grade

from tests._helpers import (
    OpaqueE2ETestCase,
    assert_boletim_envelope,
    find_bimester,
    find_subject,
    find_year,
)


class TestTier2SentinelDb(OpaqueE2ETestCase):
    def test_t2_sent_01_noop_poll_without_grade_change_creates_no_events_or_fcm(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        tok = self.unique_fcm_token("t2_sent_01")
        self.create_and_login_noturno("t2_sent_01", class_code="INF1", fcm_token=tok)
        before = self.ctx.db.get_sentinel_state("INF1")
        self.assertIsNotNone(before)
        assert before is not None
        prev_hash = before["last_class_hash"]
        prev_checked = before["last_checked_at"]

        self.ctx.db.trigger_sentinel_due_now("INF1")
        self.ctx.wait_until(
            lambda: (self.ctx.db.get_sentinel_state("INF1") or {}).get("last_checked_at")
            != prev_checked,
            timeout=8.0,
            description="sentinel poll advances last_checked_at without grade change",
        )

        after = self.ctx.db.get_sentinel_state("INF1")
        self.assertIsNotNone(after)
        assert after is not None
        self.assertEqual(after["last_class_hash"], prev_hash)
        self.assertEqual(len(self.ctx.db.get_grade_posting_events("INF1")), 0)
        self.assertEqual(self.ctx.mock_fcm.count_messages(), 0)

    def test_t2_sent_02_mb_only_change_does_not_trigger_class_hash_or_fanout(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        tok = self.unique_fcm_token("t2_sent_02")
        email, _, _, _, _, _ = self.create_and_login_noturno(
            "t2_sent_02", class_code="INF2", fcm_token=tok
        )
        before = self.ctx.db.get_sentinel_state("INF2")
        self.assertIsNotNone(before)
        assert before is not None
        prev_hash = before["last_class_hash"]
        prev_checked = before["last_checked_at"]

        # Mutate only personal M.B. while keeping class M.T. identical.
        self.ctx.mock_nsac.mutate_student_grade(
            email, 2026, "Matemática", 3, mb="9.9"
        )
        self.ctx.db.trigger_sentinel_due_now("INF2")
        self.ctx.wait_until(
            lambda: (self.ctx.db.get_sentinel_state("INF2") or {}).get("last_checked_at")
            != prev_checked,
            timeout=8.0,
            description="sentinel poll finishes after M.B.-only mutation",
        )

        after = self.ctx.db.get_sentinel_state("INF2")
        self.assertIsNotNone(after)
        assert after is not None
        self.assertEqual(after["last_class_hash"], prev_hash)
        self.assertEqual(len(self.ctx.db.get_grade_posting_events("INF2")), 0)
        self.assertEqual(self.ctx.mock_fcm.count_messages(), 0)

    def test_t2_sent_03_student_without_fcm_token_skipped_while_sibling_pushed(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        _, _, id_no_tok, _, _, _ = self.create_and_login_noturno(
            "t2_sent_03_none", class_code="ELE1", fcm_token=None
        )
        tok_b = self.unique_fcm_token("t2_sent_03_b")
        _, _, _, _, client_b, _ = self.create_and_login_noturno(
            "t2_sent_03_b", class_code="ELE1", fcm_token=tok_b
        )

        self.ctx.mock_nsac.release_class_grade(
            "ELE1", "Química", 3, mt_grade="8.4", mb_grade="9.1"
        )
        self.ctx.db.trigger_sentinel_due_now("ELE1")

        self.ctx.mock_fcm.wait_for_messages(
            1, timeout=10.0, token=tok_b, only_succeeded=True
        )
        self.assertEqual(self.ctx.mock_fcm.count_messages(), 1)

        # Verify student without fcm_token still had persisted DB grades refreshed.
        svc = self.ctx.service_client()
        resp_a = svc.get_boletim(account_id=id_no_tok)
        _, years_a = assert_boletim_envelope(resp_a)
        quim_a = find_subject(find_year(years_a, 2026), "Química")
        self.assertEqual(personal_grade(bimester_personal(find_bimester(quim_a, 3))), 9.1)

        _, years_b = assert_boletim_envelope(client_b.get_boletim())
        quim_b = find_subject(find_year(years_b, 2026), "Química")
        self.assertEqual(personal_grade(bimester_personal(find_bimester(quim_b, 3))), 9.1)

    def test_t2_sent_04_inactive_student_excluded_from_polling_and_fanout(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        tok_a = self.unique_fcm_token("t2_sent_04_inactive")
        email_a, _, _, _, _, _ = self.create_and_login_noturno(
            "t2_sent_04_inactive", class_code="ELE2", fcm_token=tok_a
        )
        tok_b = self.unique_fcm_token("t2_sent_04_active")
        self.create_and_login_noturno(
            "t2_sent_04_active", class_code="ELE2", fcm_token=tok_b
        )

        self.ctx.db.set_account_active(email_a, False)
        self.ctx.mock_nsac.clear_requests()

        self.ctx.mock_nsac.release_class_grade(
            "ELE2", "História", 3, mt_grade="8.3", mb_grade="8.9"
        )
        self.ctx.db.trigger_sentinel_due_now("ELE2")

        self.ctx.mock_fcm.wait_for_messages(
            1, timeout=10.0, token=tok_b, only_succeeded=True
        )
        self.assertEqual(self.ctx.mock_fcm.count_messages(token=tok_a), 0)
        self.assertEqual(
            self.ctx.mock_nsac.count_boletim_requests(student_email=email_a), 0
        )

    def test_t2_sent_05_transient_fcm_503_does_not_block_sibling_delivery(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        cohort = self.login_class_cohort("MEC2", 2, tag_prefix="t2_sent_05")
        tok1 = cohort[0].fcm_token
        tok2 = cohort[1].fcm_token
        assert tok1 is not None and tok2 is not None
        self.ctx.mock_fcm.set_token_error(
            tok1, status=503, error_code="UNAVAILABLE", remaining=1
        )

        self.ctx.mock_nsac.release_class_grade(
            "MEC2", "Banco de Dados", 3, mt_grade="7.9", mb_grade="8.5"
        )
        self.ctx.db.trigger_sentinel_due_now("MEC2")

        self.ctx.mock_fcm.wait_for_messages(
            1, token=tok2, only_succeeded=True, timeout=10.0
        )
        self.ctx.mock_fcm.wait_for_messages(
            1, token=tok1, only_succeeded=False, timeout=10.0
        )
        all_msgs = self.ctx.mock_fcm.get_messages(only_succeeded=False)
        self.assertTrue(
            any(m.response_status == 503 for m in all_msgs),
            f"Expected at least one 503 delivery attempt, got {all_msgs!r}",
        )
        self.assertGreaterEqual(
            self.ctx.mock_fcm.count_messages(token=tok2, only_succeeded=True), 1
        )
        self.assertEqual({m.token for m in all_msgs}, {tok1, tok2})


if __name__ == "__main__":
    unittest.main()
