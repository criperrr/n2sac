"""Tier 1 — Area B: Account, FCM token, and Discord link management."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.http_client import (
    assert_no_secrets_leaked,
    assert_success_envelope,
    get_field,
)

from tests._helpers import OpaqueE2ETestCase


class TestTier1Account(OpaqueE2ETestCase):
    def test_t1_acct_01_get_account_returns_profile_without_secrets(self) -> None:
        discord_id = self.ctx.unique_discord_id("920")
        fcm_tok = self.unique_fcm_token("t1_acct_01")
        email, password, account_id, token, client, _ = self.create_and_login_noturno(
            "t1_acct_01",
            class_code="INF1",
            discord_id=discord_id,
            fcm_token=fcm_tok,
        )

        resp = client.get_account()
        data = assert_success_envelope(resp)
        self.assertIsInstance(data, dict)
        self.assertEqual(get_field(data, "account_id", "accountId", "id"), account_id)
        self.assertEqual(get_field(data, "nsac_email", "nsacEmail", "email"), email)
        self.assertEqual(get_field(data, "class_code", "classCode"), "INF1")
        self.assertEqual(get_field(data, "discord_id", "discordId"), discord_id)
        self.assertIs(get_field(data, "active"), True)

        has_push = get_field(
            data,
            "has_fcm_token",
            "hasFcmToken",
            "push_enabled",
            "pushEnabled",
            "fcm_registered",
            "fcmRegistered",
        )
        if has_push is not None:
            self.assertTrue(bool(has_push))

        for forbidden_key in ("nsac_pass_enc", "nsac_cookies_enc", "password", "nsac_password"):
            self.assertNotIn(forbidden_key, data)
        assert_no_secrets_leaked(resp, [password, token or ""])

    def test_t1_acct_02_owner_rotates_fcm_token(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        tok_v1 = self.unique_fcm_token("t1_acct_02_v1")
        tok_v2 = self.unique_fcm_token("t1_acct_02_v2")
        email, _, _, _, client, _ = self.create_and_login_noturno(
            "t1_acct_02", class_code="INF2", fcm_token=tok_v1
        )

        put_resp = client.put_fcm_token(tok_v2)
        assert_success_envelope(put_resp)

        row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(row)
        assert row is not None
        self.assertEqual(row["fcm_token"], tok_v2)

    def test_t1_acct_03_service_links_discord_id_and_resolves_account(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        email, _, account_id, _, _, _ = self.create_and_login_noturno(
            "t1_acct_03", class_code="INF3"
        )
        discord_id = self.ctx.unique_discord_id("921")

        svc = self.ctx.service_client()
        link_resp = svc.put_discord_link(account_id, discord_id)
        assert_success_envelope(link_resp)

        row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(row)
        assert row is not None
        self.assertEqual(row["discord_id"], discord_id)

        lookup_resp = svc.get_account(discord_id=discord_id)
        lookup_data = assert_success_envelope(lookup_resp)
        self.assertEqual(get_field(lookup_data, "account_id", "accountId", "id"), account_id)

    def test_t1_acct_04_bearer_owner_self_links_discord_id(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        email, _, account_id, _, client, _ = self.create_and_login_noturno(
            "t1_acct_04", class_code="ELE1"
        )
        discord_id = self.ctx.unique_discord_id("922")

        link_resp = client.put_discord_link(account_id, discord_id)
        assert_success_envelope(link_resp)

        row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(row)
        assert row is not None
        self.assertEqual(row["discord_id"], discord_id)

    def test_t1_acct_05_service_updates_fcm_token_by_discord_id(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        discord_id = self.ctx.unique_discord_id("923")
        email, _, _, _, _, _ = self.create_and_login_noturno(
            "t1_acct_05", class_code="ELE2", discord_id=discord_id
        )
        new_tok = self.unique_fcm_token("t1_acct_05_svc")

        svc = self.ctx.service_client()
        resp = svc.put_fcm_token(new_tok, discord_id=discord_id)
        assert_success_envelope(resp)

        row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(row)
        assert row is not None
        self.assertEqual(row["fcm_token"], new_tok)


if __name__ == "__main__":
    unittest.main()
