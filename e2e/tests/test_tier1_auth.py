"""Tier 1 — Area A: Auth lifecycle, dual auth, auto-refresh, and zero-plaintext secrets."""

from __future__ import annotations

import hashlib
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

from tests._helpers import OpaqueE2ETestCase, assert_boletim_envelope


class TestTier1Auth(OpaqueE2ETestCase):
    def test_t1_auth_01_health_and_fresh_login_creates_account(self) -> None:
        self.require_managed_mocks()
        self.require_db()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.db is not None

        anon = self.ctx.api_client()
        health_resp = anon.health()
        health_data = assert_success_envelope(health_resp)
        self.assertIsNotNone(health_data)

        email = self.ctx.unique_email("t1_auth_01")
        password = "Pass_T1_Auth_01!"
        self.ctx.mock_nsac.create_noturno_student(email, password, class_code="INF1")

        login_resp = anon.login(email, password)
        login_data = assert_success_envelope(login_resp)

        account_id = int(get_field(login_data, "account_id", "accountId", "id"))
        class_code = get_field(login_data, "class_code", "classCode")
        token = login_data.get("token")
        expires_at = get_field(login_data, "expires_at", "expiresAt")

        self.assertGreater(account_id, 0)
        self.assertEqual(class_code, "INF1")
        self.assertIsInstance(token, str)
        self.assertTrue(len(str(token)) > 0)
        self.assertTrue(bool(expires_at))

        row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(row)
        assert row is not None
        self.assertEqual(row["id"], account_id)
        self.assertEqual(row["class_code"], "INF1")
        self.assertTrue(row["active"])

    def test_t1_auth_02_relogin_is_idempotent_and_updates_fcm_token(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        fcm_v1 = self.unique_fcm_token("t1_auth_02_v1")
        fcm_v2 = self.unique_fcm_token("t1_auth_02_v2")
        email, password, account_id_1, token_1, _, _ = self.create_and_login_noturno(
            "t1_auth_02", class_code="INF2", fcm_token=fcm_v1
        )

        relogin_resp = self.ctx.api_client().login(email, password, fcm_token=fcm_v2)
        relogin_data = assert_success_envelope(relogin_resp)
        account_id_2 = int(get_field(relogin_data, "account_id", "accountId", "id"))
        token_2 = relogin_data.get("token")

        self.assertEqual(account_id_2, account_id_1)
        self.assertIsInstance(token_2, str)
        self.assertNotEqual(token_2, token_1)
        self.assertEqual(self.ctx.db.count_accounts(email), 1)

        row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(row)
        assert row is not None
        self.assertEqual(row["fcm_token"], fcm_v2)

    def test_t1_auth_03_bearer_auth_and_logout_revokes_session(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        _, _, account_id, token, client, _ = self.create_and_login_noturno(
            "t1_auth_03", class_code="INF3"
        )
        self.assertIsNotNone(token)
        assert token is not None

        acct_resp = client.get_account()
        acct_data = assert_success_envelope(acct_resp)
        self.assertEqual(
            get_field(acct_data, "account_id", "accountId", "id"),
            account_id,
        )

        logout_resp = client.logout()
        assert_success_envelope(logout_resp)

        token_hash = hashlib.sha256(token.encode("utf-8")).hexdigest()
        sessions = self.ctx.db.get_user_sessions(account_id)
        matching = [s for s in sessions if s["token_hash"] == token_hash]
        self.assertEqual(len(matching), 1)
        self.assertIsNotNone(matching[0]["revoked_at"])

    def test_t1_auth_04_service_auth_header_mode_targets_account_and_discord(self) -> None:
        discord_id = self.ctx.unique_discord_id("910")
        _, _, account_id, _, _, _ = self.create_and_login_noturno(
            "t1_auth_04", class_code="ELE1", discord_id=discord_id
        )

        svc = self.ctx.service_client(mode="header")
        acct_resp = svc.get_account(account_id=account_id)
        acct_data = assert_success_envelope(acct_resp)
        self.assertEqual(get_field(acct_data, "account_id", "accountId", "id"), account_id)

        boletim_resp = svc.get_boletim(discord_id=discord_id)
        _, years = assert_boletim_envelope(boletim_resp)
        self.assertEqual(len(years), 1)

    def test_t1_auth_05_service_auth_bearer_service_mode(self) -> None:
        _, _, account_id, _, _, _ = self.create_and_login_noturno(
            "t1_auth_05", class_code="ELE2"
        )

        svc = self.ctx.service_client(mode="bearer_service")
        acct_resp = svc.get_account(account_id=account_id)
        acct_data = assert_success_envelope(acct_resp)
        self.assertEqual(get_field(acct_data, "account_id", "accountId", "id"), account_id)

        home_resp = svc.get_home(account_id=account_id)
        home_data = assert_success_envelope(home_resp)
        self.assertIsInstance(home_data, dict)

    def test_t1_auth_06_zero_plaintext_secrets_in_db_and_http(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        password = "UltraSecret_T1_Pass_987!"
        email = self.ctx.unique_email("t1_auth_06")
        assert self.ctx.mock_nsac is not None
        self.ctx.mock_nsac.create_noturno_student(email, password, class_code="MEC1")

        login_resp = self.ctx.api_client().login(email, password)
        login_data = assert_success_envelope(login_resp)
        account_id = int(get_field(login_data, "account_id", "accountId", "id"))
        token = str(login_data["token"])
        client = self.ctx.api_client(bearer_token=token)

        acct_resp = client.get_account()
        assert_success_envelope(acct_resp)
        boletim_resp = client.get_boletim()
        assert_boletim_envelope(boletim_resp)

        forbidden = [
            password,
            "SYNTHETIC_SESS_",
            "SYNTHETIC_XSRF_",
            self.ctx.config.encryption_key,
            self.ctx.config.service_api_key,
        ]
        assert_no_secrets_leaked(login_resp, forbidden)
        assert_no_secrets_leaked(acct_resp, forbidden + [token])
        assert_no_secrets_leaked(boletim_resp, forbidden + [token])

        row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(row)
        assert row is not None
        pass_enc = str(row["nsac_pass_enc"])
        cookies_enc = str(row["nsac_cookies_enc"])

        self.assertTrue(pass_enc.startswith("v1:"), f"Expected v1: prefix, got {pass_enc!r}")
        self.assertTrue(cookies_enc.startswith("v1:"), f"Expected v1: prefix, got {cookies_enc!r}")
        for secret in (password, "SYNTHETIC_SESS_", "SYNTHETIC_XSRF_"):
            self.assertNotIn(secret, pass_enc)
            self.assertNotIn(secret, cookies_enc)

        sessions = self.ctx.db.get_user_sessions(account_id)
        expected_hash = hashlib.sha256(token.encode("utf-8")).hexdigest()
        self.assertTrue(any(s["token_hash"] == expected_hash for s in sessions))
        for s in sessions:
            self.assertNotEqual(s["token_hash"], token)

    def test_t1_auth_07_transparent_cookie_refresh_on_expired_nsac_session(self) -> None:
        self.require_managed_mocks()
        self.require_db()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.db is not None

        email, _, _, _, client, _ = self.create_and_login_noturno(
            "t1_auth_07", class_code="MEC2"
        )
        before_row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(before_row)
        assert before_row is not None
        cookies_before = before_row["nsac_cookies_enc"]

        invalidated = self.ctx.mock_nsac.expire_student_sessions(email)
        self.assertGreaterEqual(invalidated, 1)

        home_resp = client.get_home()
        home_data = assert_success_envelope(home_resp)
        self.assertIsInstance(home_data, dict)

        after_row = self.ctx.db.get_account_row(email)
        self.assertIsNotNone(after_row)
        assert after_row is not None
        self.assertNotEqual(after_row["nsac_cookies_enc"], cookies_before)
        self.assertTrue(str(after_row["nsac_cookies_enc"]).startswith("v1:"))


if __name__ == "__main__":
    unittest.main()
