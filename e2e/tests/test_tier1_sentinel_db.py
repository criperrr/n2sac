"""Tier 1 — Area F: Sentinel init, normalized DB tables, poll hints, FCM push, aux snapshots."""

from __future__ import annotations

import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.http_client import assert_success_envelope

from tests._helpers import OpaqueE2ETestCase, assert_boletim_envelope


class TestTier1SentinelDb(OpaqueE2ETestCase):
    def test_t1_sent_01_login_initializes_sentinel_class_state(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        self.create_and_login_noturno("t1_sent_01", class_code="MEC3")
        state = self.ctx.db.get_sentinel_state("MEC3")
        self.assertIsNotNone(state)
        assert state is not None
        self.assertEqual(state["class_code"], "MEC3")
        self.assertTrue(bool(state["last_class_hash"]))
        self.assertGreater(int(state["current_interval_secs"]), 0)
        self.assertIsNotNone(state["next_check_at"])

    def test_t1_sent_02_login_populates_normalized_boletim_tables(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        _, _, account_id, _, _, _ = self.create_and_login_noturno(
            "t1_sent_02", class_code="INF1"
        )
        counts = self.ctx.db.count_boletim_tables_for_account(account_id)
        self.assertEqual(counts["accounts_school_years"], 1)
        self.assertEqual(counts["bimesters"], 4)
        self.assertEqual(counts["grades"], 28)
        self.assertEqual(counts["subject_final_results"], 7)

    def test_t1_sent_03_cold_baseline_vs_historical_window_poll_hints(self) -> None:
        def seed_historical(db) -> None:  # type: ignore[no-untyped-def]
            db.seed_grade_posting_events("ELI2", offset_days=-1, years_back=1, count=3)

        self.reset_isolated_state(seed_fn=seed_historical)
        assert self.ctx.db is not None

        _, _, _, _, cold_client, _ = self.create_and_login_noturno(
            "t1_sent_03_cold", class_code="ELI1"
        )
        _, _, _, _, hist_client, _ = self.create_and_login_noturno(
            "t1_sent_03_hist", class_code="ELI2"
        )

        # Sample sentinel_class_state right after login (before get_boletim applies any API bump).
        cold_before = self.ctx.db.get_sentinel_state("ELI1")
        hist_before = self.ctx.db.get_sentinel_state("ELI2")
        self.assertIsNotNone(cold_before)
        self.assertIsNotNone(hist_before)
        assert cold_before is not None and hist_before is not None

        self.assertEqual(float(cold_before["current_certainty"]), 0.0)
        self.assertEqual(
            int(cold_before["current_interval_secs"]),
            self.ctx.config.sentinel_cold_baseline_secs,
        )
        self.assertGreater(float(hist_before["current_certainty"]), 0.0)
        self.assertLess(
            int(hist_before["current_interval_secs"]),
            int(cold_before["current_interval_secs"]),
        )

        cold_resp = cold_client.get_boletim()
        assert_boletim_envelope(cold_resp, require_poll_hint=True)
        hist_resp = hist_client.get_boletim()
        assert_boletim_envelope(hist_resp, require_poll_hint=True)

        cold_after = self.ctx.db.get_sentinel_state("ELI1")
        hist_after = self.ctx.db.get_sentinel_state("ELI2")
        self.assertIsNotNone(cold_after)
        self.assertIsNotNone(hist_after)
        assert cold_after is not None and hist_after is not None

        self.assertGreaterEqual(float(cold_after["current_certainty"]), 0.0)
        self.assertLessEqual(float(cold_after["current_certainty"]), 0.16)
        self.assertLess(
            float(cold_after["current_certainty"]),
            float(hist_after["current_certainty"]),
        )
        self.assertGreater(
            int(cold_after["current_interval_secs"]),
            int(hist_after["current_interval_secs"]),
        )
        self.assertLess(
            int(hist_resp.json_body["next_poll_hint_secs"]),
            int(cold_resp.json_body["next_poll_hint_secs"]),
        )

    def test_t1_sent_04_single_class_grade_release_triggers_fcm_push_and_event(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        fcm_tok = self.unique_fcm_token("t1_sent_04")
        self.create_and_login_noturno("t1_sent_04", class_code="MEI1", fcm_token=fcm_tok)

        updated = self.ctx.mock_nsac.release_class_grade(
            "MEI1", "Matemática", 3, mt_grade="8.2", mb_grade="9.0"
        )
        self.assertEqual(updated, 1)
        self.ctx.db.trigger_sentinel_due_now("MEI1")

        msgs = self.ctx.mock_fcm.wait_for_messages(
            1, timeout=10.0, token=fcm_tok, only_succeeded=True
        )
        self.assertEqual(len(msgs), 1)
        msg = msgs[0]
        self.assertEqual(msg.bearer_token, "mock-fcm-oauth2-access-token")
        self.assertIsInstance(msg.notification, dict)
        assert msg.notification is not None
        title = msg.notification.get("title")
        body = msg.notification.get("body")
        self.assertIsInstance(title, str)
        self.assertIsInstance(body, str)
        self.assertTrue(bool(str(title).strip()))
        self.assertTrue(bool(str(body).strip()))
        self.assertIsInstance(msg.data, dict)
        assert msg.data is not None
        data_repr = repr(msg.data)
        self.assertTrue(
            "Matemática" in data_repr or "3" in data_repr,
            f"Expected subject or bimester in FCM data payload, got {msg.data!r}",
        )

        events = self.ctx.db.get_grade_posting_events("MEI1")
        self.assertGreaterEqual(len(events), 1)

    def test_t1_sent_05_auxiliary_routes_persist_snapshots_in_db(self) -> None:
        self.require_db()
        assert self.ctx.db is not None

        _, _, account_id, _, client, _ = self.create_and_login_noturno(
            "t1_sent_05", class_code="MEI2"
        )
        assert_success_envelope(client.get_home())
        assert_success_envelope(client.get_atrasos())
        assert_success_envelope(client.get_saidas())
        assert_success_envelope(client.get_ficha_list())

        rows = self.ctx.db.query_json(
            "SELECT COALESCE(json_agg(r ORDER BY r.route_key), '[]'::json) FROM ("
            "SELECT route_key, etag, payload_json "
            f"FROM auxiliary_snapshots WHERE account_id = {account_id}"
            ") r;"
        )
        self.assertIsInstance(rows, list)
        self.assertGreaterEqual(len(rows), 4)
        route_keys = {str(r["route_key"]) for r in rows}
        for expected in ("home", "atrasos", "saidas", "ficha"):
            self.assertTrue(
                any(expected in rk for rk in route_keys),
                f"Expected route_key containing {expected!r} in {route_keys!r}",
            )
        for row in rows:
            self.assertTrue(bool(row["etag"]))
            self.assertIsNotNone(row["payload_json"])


if __name__ == "__main__":
    unittest.main()
