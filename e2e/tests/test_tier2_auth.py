"""Tier 2 — Area A: Auth, dual auth, and crypto boundary/fault cases."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.http_client import assert_error_envelope, assert_no_secrets_leaked

from tests._helpers import OpaqueE2ETestCase


class TestTier2Auth(OpaqueE2ETestCase):
    def test_t2_auth_01_wrong_password_or_unknown_email_returns_401_no_account(self) -> None:
        self.require_managed_mocks()
        self.require_db()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.db is not None

        email = self.ctx.unique_email("t2_auth_01")
        real_pw = "RealPass_T2_01!"
        bad_pw = "WrongPass_T2_01!"
        self.ctx.mock_nsac.create_noturno_student(email, real_pw, class_code="INF1")

        anon = self.ctx.api_client()
        r_bad_pw = anon.login(email, bad_pw)
        assert_error_envelope(r_bad_pw, 401, {"INVALID_NSAC_CREDENTIALS"})
        assert_no_secrets_leaked(r_bad_pw, [real_pw, bad_pw])
        self.assertEqual(self.ctx.db.count_accounts(email), 0)

        unknown_email = self.ctx.unique_email("t2_auth_01_unknown")
        r_unknown = anon.login(unknown_email, bad_pw)
        assert_error_envelope(r_unknown, 401, {"INVALID_NSAC_CREDENTIALS"})
        assert_no_secrets_leaked(r_unknown, [bad_pw])
        self.assertEqual(self.ctx.db.count_accounts(unknown_email), 0)

    def test_t2_auth_02_empty_credentials_or_malformed_json_returns_400(self) -> None:
        anon = self.ctx.api_client()

        r_empty = anon.login("", "")
        assert_error_envelope(r_empty, 400, {"INVALID_PARAMS"})

        r_malformed = anon.post(
            "/api/v1/auth/login",
            raw_body=b"{not-valid-json",
            content_type="application/json",
        )
        assert_error_envelope(r_malformed, (400, 422))

    def test_t2_auth_03_missing_malformed_or_unknown_auth_returns_401(self) -> None:
        anon = self.ctx.api_client()
        assert_error_envelope(anon.get_account(), 401, {"UNAUTHORIZED"})
        assert_error_envelope(anon.get_boletim(), 401, {"UNAUTHORIZED"})

        malformed = self.ctx.api_client().get(
            "/api/v1/account",
            headers={"Authorization": "Basic dXNlcjpwYXNz"},
        )
        assert_error_envelope(malformed, 401, {"UNAUTHORIZED"})

        unknown_bearer = self.ctx.api_client(bearer_token="nonexistent-token-xyz-000")
        assert_error_envelope(unknown_bearer.get_account(), 401, {"UNAUTHORIZED"})

    def test_t2_auth_04_expired_and_revoked_bearer_tokens_return_401(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        email, password, _, token_1, client_1, _ = self.create_and_login_noturno(
            "t2_auth_04", class_code="INF2"
        )
        self.assertIsNotNone(token_1)
        assert token_1 is not None

        self.ctx.db.expire_user_session(token_1)
        assert_error_envelope(client_1.get_account(), 401, {"UNAUTHORIZED"})

        relogin = self.ctx.api_client().login(email, password)
        token_2 = relogin.json_body["data"]["token"]
        client_2 = self.ctx.api_client(bearer_token=token_2)
        logout_resp = client_2.logout()
        self.assertEqual(logout_resp.status, 200)

        assert_error_envelope(client_2.get_boletim(), 401, {"UNAUTHORIZED"})

    def test_t2_auth_05_bearer_user_cannot_target_another_account_403(self) -> None:
        discord_b = self.ctx.unique_discord_id("930")
        _, _, _, _, client_a, _ = self.create_and_login_noturno(
            "t2_auth_05_a", class_code="INF1"
        )
        _, _, account_id_b, _, _, _ = self.create_and_login_noturno(
            "t2_auth_05_b", class_code="INF2", discord_id=discord_b
        )

        r_acct_id = client_a.get_boletim(account_id=account_id_b)
        assert_error_envelope(r_acct_id, 403, {"FORBIDDEN"})

        r_discord = client_a.get_account(discord_id=discord_b)
        assert_error_envelope(r_discord, 403, {"FORBIDDEN"})

    def test_t2_auth_06_invalid_inactive_or_under_scoped_service_key(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        _, _, account_id, _, _, _ = self.create_and_login_noturno(
            "t2_auth_06", class_code="INF3"
        )

        bad_svc = self.ctx.api_client(service_key="totally-wrong-service-key")
        assert_error_envelope(
            bad_svc.get_account(account_id=account_id),
            (401, 403),
            {"UNAUTHORIZED", "FORBIDDEN"},
        )

        disabled_name = "e2e-disabled-bot"
        disabled_key = "e2e-disabled-secret-key-00"
        self.addCleanup(
            self.ctx.db.execute,
            f"DELETE FROM service_clients WHERE name = '{disabled_name}';",
        )
        self.ctx.db.seed_service_client(disabled_name, disabled_key)
        self.ctx.db.set_service_client_active(disabled_name, False)
        inactive_resp = self.ctx.api_client(service_key=disabled_key).get_account(
            account_id=account_id
        )
        assert_error_envelope(inactive_resp, (401, 403), {"UNAUTHORIZED", "FORBIDDEN"})

        ro_name = "e2e_readonly_scope_client"
        ro_key = "e2e-readonly-secret-key-99"
        self.addCleanup(
            self.ctx.db.execute,
            f"DELETE FROM service_clients WHERE name = '{ro_name}';",
        )
        self.ctx.db.seed_service_client(ro_name, ro_key, scopes=["nsac:read"])
        ro_client = self.ctx.api_client(service_key=ro_key)
        self.assertEqual(ro_client.get_account(account_id=account_id).status, 200)

        denied_write = ro_client.put_discord_link(
            account_id, self.ctx.unique_discord_id("931")
        )
        assert_error_envelope(denied_write, 403, {"FORBIDDEN"})

    def test_t2_auth_07_service_missing_or_unknown_target_returns_400_or_404(self) -> None:
        svc = self.ctx.service_client()

        r_missing = svc.get_boletim()
        assert_error_envelope(r_missing, 400, {"INVALID_PARAMS"})

        r_unknown_id = svc.get_account(account_id=999_999_999)
        assert_error_envelope(r_unknown_id, 404, {"USER_NOT_LINKED", "NOT_FOUND"})

        r_unknown_discord = svc.get_boletim(discord_id="999999999999999")
        assert_error_envelope(r_unknown_discord, 404, {"USER_NOT_LINKED", "NOT_FOUND"})

    def test_t2_auth_08_deactivated_account_blocked_for_bearer_and_service(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        email, _, account_id, _, client, _ = self.create_and_login_noturno(
            "t2_auth_08", class_code="ELE1"
        )
        self.ctx.db.set_account_active(email, False)

        assert_error_envelope(
            client.get_account(), (401, 403), {"UNAUTHORIZED", "FORBIDDEN"}
        )
        assert_error_envelope(
            self.ctx.service_client().get_boletim(account_id=account_id),
            (401, 403, 404),
            {"UNAUTHORIZED", "FORBIDDEN", "USER_NOT_LINKED"},
        )

    def test_t2_auth_09_expired_nsac_session_with_changed_password_fails_refresh(self) -> None:
        self.require_managed_mocks()
        assert self.ctx.mock_nsac is not None

        old_pw = "OldPass_T2_09!"
        new_pw = "NewChangedPass_T2_09!"
        email, _, _, token, client, _ = self.create_and_login_noturno(
            "t2_auth_09", class_code="ELE2", password=old_pw
        )

        self.ctx.mock_nsac.expire_student_sessions(email)
        self.ctx.mock_nsac.set_student_password(email, new_pw)

        resp = client.get_home()
        assert_error_envelope(
            resp,
            (401, 502),
            {"NSAC_AUTH_EXPIRED", "SESSION_REFRESH_ERROR", "INVALID_NSAC_CREDENTIALS"},
        )
        assert_no_secrets_leaked(resp, [old_pw, new_pw, token or ""])


if __name__ == "__main__":
    unittest.main()
