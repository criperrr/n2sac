"""Tier 2 — Area B: Account, FCM token, Discord link, and UNREGISTERED push cleanup."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.http_client import assert_error_envelope

from tests._helpers import OpaqueE2ETestCase


class TestTier2Account(OpaqueE2ETestCase):
    def test_t2_acct_01_bearer_cross_user_put_returns_403_without_mutation(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        _, _, _, _, client_a, _ = self.create_and_login_noturno(
            "t2_acct_01_a", class_code="INF1"
        )
        tok_b = self.unique_fcm_token("t2_acct_01_b")
        disc_b = self.ctx.unique_discord_id("940")
        email_b, _, id_b, _, _, _ = self.create_and_login_noturno(
            "t2_acct_01_b", class_code="INF2", fcm_token=tok_b, discord_id=disc_b
        )

        r_fcm = client_a.put_fcm_token("hijack-token", account_id=id_b)
        assert_error_envelope(r_fcm, 403, {"FORBIDDEN"})

        r_disc = client_a.put_discord_link(id_b, self.ctx.unique_discord_id("941"))
        assert_error_envelope(r_disc, 403, {"FORBIDDEN"})

        row_b = self.ctx.db.get_account_row(email_b)
        self.assertIsNotNone(row_b)
        assert row_b is not None
        self.assertEqual(row_b["fcm_token"], tok_b)
        self.assertEqual(row_b["discord_id"], disc_b)

    def test_t2_acct_02_put_fcm_token_empty_or_missing_returns_400(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno(
            "t2_acct_02", class_code="INF3"
        )

        assert_error_envelope(client.put_fcm_token(""), 400, {"INVALID_PARAMS"})
        assert_error_envelope(client.put_fcm_token("   "), 400, {"INVALID_PARAMS"})
        assert_error_envelope(
            client.put("/api/v1/account/fcm-token", json_body={}),
            (400, 422),
        )

    def test_t2_acct_03_put_discord_link_empty_or_unknown_account(self) -> None:
        _, _, account_id, _, client, _ = self.create_and_login_noturno(
            "t2_acct_03", class_code="ELE1"
        )

        assert_error_envelope(
            client.put_discord_link(account_id, ""), 400, {"INVALID_PARAMS"}
        )
        assert_error_envelope(
            self.ctx.service_client().put_discord_link(
                999_999_998, self.ctx.unique_discord_id("942")
            ),
            404,
            {"USER_NOT_LINKED", "NOT_FOUND"},
        )

    def test_t2_acct_04_duplicate_discord_id_conflict_rejected_cleanly(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        shared_discord = self.ctx.unique_discord_id("943")
        email_a, _, _, _, _, _ = self.create_and_login_noturno(
            "t2_acct_04_a", class_code="ELE2", discord_id=shared_discord
        )
        _, _, id_b, _, client_b, _ = self.create_and_login_noturno(
            "t2_acct_04_b", class_code="ELE3"
        )

        conflict_resp = client_b.put_discord_link(id_b, shared_discord)
        assert_error_envelope(
            conflict_resp, (400, 409), {"INVALID_PARAMS", "CONFLICT"}
        )

        row_a = self.ctx.db.get_account_row(email_a)
        self.assertIsNotNone(row_a)
        assert row_a is not None
        self.assertEqual(row_a["discord_id"], shared_discord)

    def test_t2_acct_05_service_lookup_unlinked_discord_id_returns_404(self) -> None:
        resp = self.ctx.service_client().get_account(
            discord_id=self.ctx.unique_discord_id("999")
        )
        assert_error_envelope(resp, 404, {"USER_NOT_LINKED", "NOT_FOUND"})

    def test_t2_acct_06_fcm_404_unregistered_clears_stale_token_in_db(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        stale_tok = self.unique_fcm_token("t2_acct_06_stale")
        email, _, _, _, _, _ = self.create_and_login_noturno(
            "t2_acct_06", class_code="MEC1", fcm_token=stale_tok
        )
        self.ctx.mock_fcm.set_token_error(stale_tok, status=404, error_code="UNREGISTERED")

        self.ctx.mock_nsac.release_class_grade(
            "MEC1", "Física", 3, mt_grade="8.1", mb_grade="8.7"
        )
        self.ctx.db.trigger_sentinel_due_now("MEC1")

        captured = self.ctx.mock_fcm.wait_for_messages(
            1, timeout=10.0, token=stale_tok, only_succeeded=False
        )
        self.assertEqual(captured[0].response_status, 404)
        self.assertEqual(captured[0].error_code, "UNREGISTERED")

        self.ctx.wait_until(
            lambda: (self.ctx.db.get_account_row(email) or {}).get("fcm_token") is None,
            timeout=8.0,
            description="stale fcm_token cleared to NULL in PostgreSQL",
        )


if __name__ == "__main__":
    unittest.main()
