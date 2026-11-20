"""Tier 4 — Real-world CTI 21-class application scenarios & adversarial workloads."""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import unittest

if __package__ in (None, ""):
    from pathlib import Path
    import sys

    _e2e_root = str(Path(__file__).resolve().parents[1])
    if _e2e_root not in sys.path:
        sys.path.insert(0, _e2e_root)

from harness.mock_nsac import CTI_CLASSES

from tests._helpers import OpaqueE2ETestCase, assert_boletim_envelope


class TestTier4CtiScenarios(OpaqueE2ETestCase):
    def test_t4_cti_01_historical_window_acceleration_vs_cold_baseline_and_hot_decay(self) -> None:
        def seed_historical(db) -> None:  # type: ignore[no-untyped-def]
            db.seed_grade_posting_events("INI1B", offset_days=-1, years_back=1, count=3)

        self.reset_isolated_state(seed_fn=seed_historical)
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        tok_cold = self.unique_fcm_token("t4_cti_01_cold")
        _, _, _, _, cold_client, _ = self.create_and_login_diurno(
            "t4_cti_01_cold", class_code="INI1A", fcm_token=tok_cold
        )
        _, _, _, _, hist_client, _ = self.create_and_login_diurno(
            "t4_cti_01_hist", class_code="INI1B"
        )

        # Sample sentinel_class_state right after login (before get_boletim applies any API bump).
        cold_before = self.ctx.db.get_sentinel_state("INI1A")
        hist_before = self.ctx.db.get_sentinel_state("INI1B")
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

        cold_after = self.ctx.db.get_sentinel_state("INI1A")
        hist_after = self.ctx.db.get_sentinel_state("INI1B")
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

        # Release a grade in INI1A to trigger Hot / Burst Mode, then verify decay after hot_duration_secs (10s).
        self.ctx.mock_nsac.release_class_grade(
            "INI1A", "Matemática", 3, mt_grade="8.3", mb_grade="8.9"
        )
        self.ctx.db.trigger_sentinel_due_now("INI1A")
        self.ctx.mock_fcm.wait_for_messages(
            1, timeout=10.0, token=tok_cold, only_succeeded=True
        )

        hot_state = self.ctx.db.get_sentinel_state("INI1A")
        self.assertIsNotNone(hot_state)
        assert hot_state is not None
        self.assertIsNotNone(hot_state["hot_until"])
        self.assertLessEqual(
            int(hot_state["current_interval_secs"]),
            self.ctx.config.sentinel_historical_interval_secs,
        )

        # Wait until hot window expires and interval decays upward (> hot_interval_secs).
        self.ctx.wait_until(
            lambda: int((self.ctx.db.get_sentinel_state("INI1A") or {}).get("current_interval_secs", 0))
            > self.ctx.config.sentinel_hot_interval_secs,
            timeout=float(self.ctx.config.sentinel_hot_duration_secs) + 8.0,
            interval=0.3,
            description="INI1A hot burst interval decays after hot_until expires",
        )

    def test_t4_cti_02_concurrent_api_flood_cooldown_and_moka_cache_shield(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.db is not None

        _, _, _, _, client, _ = self.create_and_login_noturno(
            "t4_cti_02", class_code="MEC1"
        )
        assert_boletim_envelope(client.get_boletim())
        self.ctx.mock_nsac.clear_requests()

        def call_boletim(_: int) -> int:
            resp = client.get_boletim()
            assert_boletim_envelope(resp)
            return resp.status

        with ThreadPoolExecutor(max_workers=12) as pool:
            statuses = list(pool.map(call_boletim, range(60)))

        self.assertEqual(len(statuses), 60)
        self.assertTrue(all(s == 200 for s in statuses))
        self.assertLessEqual(
            self.ctx.mock_nsac.count_boletim_requests(class_code="MEC1"), 2
        )

        state = self.ctx.db.get_sentinel_state("MEC1")
        self.assertIsNotNone(state)
        assert state is not None
        self.assertLessEqual(float(state["current_certainty"]), 0.16)
        self.assertGreater(
            int(state["current_interval_secs"]),
            self.ctx.config.sentinel_historical_interval_secs,
        )

    def test_t4_cti_03_nsac_maintenance_503_and_timeout_recovery_no_poisoning(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        cohort = self.login_class_cohort("ELI2", 2, tag_prefix="t4_cti_03")
        baseline_state = self.ctx.db.get_sentinel_state("ELI2")
        self.assertIsNotNone(baseline_state)
        assert baseline_state is not None
        baseline_hash = baseline_state["last_class_hash"]

        self.ctx.mock_nsac.set_maintenance("503")
        for expected_err in (1, 2, 3):
            self.ctx.db.trigger_sentinel_due_now("ELI2")
            self.ctx.wait_until(
                lambda target=expected_err: int(
                    (self.ctx.db.get_sentinel_state("ELI2") or {}).get("consecutive_errors", 0)
                )
                >= target,
                timeout=8.0,
                description=f"ELI2 consecutive_errors >= {expected_err} on 503",
            )

        self.ctx.mock_nsac.set_maintenance("delay", delay_secs=6.0)
        self.ctx.db.trigger_sentinel_due_now("ELI2")
        self.ctx.wait_until(
            lambda: int(
                (self.ctx.db.get_sentinel_state("ELI2") or {}).get("consecutive_errors", 0)
            )
            >= 4,
            timeout=10.0,
            description="ELI2 consecutive_errors >= 4 after NSAC timeout",
        )

        during_state = self.ctx.db.get_sentinel_state("ELI2")
        self.assertIsNotNone(during_state)
        assert during_state is not None
        self.assertEqual(during_state["last_class_hash"], baseline_hash)
        self.assertEqual(len(self.ctx.db.get_grade_posting_events("ELI2")), 0)
        self.assertEqual(self.ctx.mock_fcm.count_messages(), 0)

        self.ctx.mock_nsac.set_maintenance("off")
        self.ctx.mock_nsac.release_class_grade(
            "ELI2", "Matemática", 3, mt_grade="8.5", mb_grade="9.3"
        )
        self.ctx.db.trigger_sentinel_due_now("ELI2")

        for member in cohort:
            self.ctx.mock_fcm.wait_for_messages(
                1, timeout=10.0, token=member.fcm_token, only_succeeded=True
            )

        self.ctx.wait_until(
            lambda: int(
                (self.ctx.db.get_sentinel_state("ELI2") or {}).get("consecutive_errors", -1)
            )
            == 0,
            timeout=8.0,
            description="ELI2 consecutive_errors resets to 0 on recovery",
        )
        recovered = self.ctx.db.get_sentinel_state("ELI2")
        self.assertIsNotNone(recovered)
        assert recovered is not None
        self.assertNotEqual(recovered["last_class_hash"], baseline_hash)
        self.assertGreaterEqual(len(self.ctx.db.get_grade_posting_events("ELI2")), 1)

    def test_t4_cti_04_sentinel_cookie_and_password_failover_during_hot_window(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        cohort = self.login_class_cohort("MEI2", 2, tag_prefix="t4_cti_04")

        # Trigger initial grade release to put MEI2 into Hot Mode.
        self.ctx.mock_nsac.release_class_grade(
            "MEI2", "Matemática", 3, mt_grade="7.7", mb_grade="8.1"
        )
        self.ctx.db.trigger_sentinel_due_now("MEI2")
        for member in cohort:
            self.ctx.mock_fcm.wait_for_messages(
                1, timeout=10.0, token=member.fcm_token, only_succeeded=True
            )

        hot_state = self.ctx.db.get_sentinel_state("MEI2")
        self.assertIsNotNone(hot_state)
        assert hot_state is not None
        self.assertIsNotNone(hot_state["hot_until"])

        active_id_raw = self.ctx.db.query_scalar(
            "SELECT COALESCE(active_sentinel_account_id::text, '') "
            "FROM sentinel_class_state WHERE class_code = 'MEI2';"
        )
        if active_id_raw.isdigit() and int(active_id_raw) == cohort[1].account_id:
            primary, backup = cohort[1], cohort[0]
        else:
            primary, backup = cohort[0], cohort[1]

        # Break primary sentinel's NSAC session and password so refresh fails.
        self.ctx.mock_nsac.expire_student_sessions(primary.email)
        self.ctx.mock_nsac.set_student_password(primary.email, "Changed_Failover_999!")
        self.ctx.mock_fcm.reset()

        self.ctx.mock_nsac.release_class_grade(
            "MEI2", "Física", 3, mt_grade="8.4", mb_grade="9.0"
        )
        self.ctx.db.trigger_sentinel_due_now("MEI2")

        self.ctx.mock_fcm.wait_for_messages(
            1, timeout=12.0, token=backup.fcm_token, only_succeeded=True
        )
        events = self.ctx.db.get_grade_posting_events("MEI2")
        self.assertGreaterEqual(len(events), 2)

        failover_id_raw = self.ctx.db.query_scalar(
            "SELECT COALESCE(active_sentinel_account_id::text, '') "
            "FROM sentinel_class_state WHERE class_code = 'MEI2';"
        )
        if failover_id_raw.isdigit():
            self.assertEqual(int(failover_id_raw), backup.account_id)

    def test_t4_cti_05_multi_class_thundering_herd_and_fanout_isolation(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        mutated_classes = ("INI1A", "INF1", "MEC3")
        untouched_classes = ("INI2A", "ELI1", "MEI1")

        members_by_class = {}
        for cls in mutated_classes + untouched_classes:
            is_diurno = cls.startswith(("INI", "ELI", "MEI"))
            members_by_class[cls] = self.login_class_cohort(
                cls, 3, tag_prefix="t4_cti_05", diurno=is_diurno
            )

        self.ctx.mock_nsac.clear_requests()

        for cls in mutated_classes:
            self.ctx.mock_nsac.release_class_grade(
                cls, "Matemática", 3, mt_grade="8.6", mb_grade="9.2"
            )
        self.ctx.db.trigger_sentinel_due_now(None)

        msgs = self.ctx.mock_fcm.wait_for_messages(9, timeout=15.0, only_succeeded=True)
        self.assertEqual(len(msgs), 9)

        expected_tokens = {
            m.fcm_token
            for cls in mutated_classes
            for m in members_by_class[cls]
        }
        self.assertEqual({m.token for m in msgs}, expected_tokens)

        for cls in untouched_classes:
            for member in members_by_class[cls]:
                self.assertEqual(
                    self.ctx.mock_fcm.count_messages(token=member.fcm_token), 0
                )
            # At most 1 sentinel poll for untouched class, zero fan-out across its 3 students.
            self.assertLessEqual(
                self.ctx.mock_nsac.count_boletim_requests(class_code=cls), 1
            )

        events = self.ctx.db.get_grade_posting_events()
        event_classes = {e["class_code"] for e in events}
        self.assertEqual(event_classes, set(mutated_classes))

    def test_t4_cti_06_o21_full_school_steady_state_efficiency(self) -> None:
        self.reset_isolated_state()
        assert self.ctx.mock_nsac is not None
        assert self.ctx.mock_fcm is not None
        assert self.ctx.db is not None

        seeded = self.ctx.mock_nsac.seed_cti_classes()
        self.assertEqual(len(seeded), 21)

        tokens_by_class: dict[str, list[str]] = {cls: [] for cls in CTI_CLASSES}
        for cls in CTI_CLASSES:
            email, password = seeded[cls]
            tok = self.unique_fcm_token(f"t4_cti_06_{cls}")
            tokens_by_class[cls].append(tok)
            resp = self.ctx.api_client().login(email, password, fcm_token=tok)
            self.assertEqual(resp.status, 200)

        # Add 4 extra classmates to INF3 (25 accounts total across 21 CTI classes).
        extra_inf3 = self.login_class_cohort("INF3", 4, tag_prefix="t4_cti_06_extra")
        for member in extra_inf3:
            assert member.fcm_token is not None
            tokens_by_class["INF3"].append(member.fcm_token)

        self.ctx.wait_until(
            lambda: all(self.ctx.db.get_sentinel_state(cls) is not None for cls in CTI_CLASSES),
            timeout=10.0,
            description="all 21 CTI classes initialized in sentinel_class_state",
        )

        prev_checked = {
            cls: (self.ctx.db.get_sentinel_state(cls) or {}).get("last_checked_at")
            for cls in CTI_CLASSES
        }
        self.ctx.mock_nsac.clear_requests()
        self.ctx.db.trigger_sentinel_due_now(None)

        self.ctx.wait_until(
            lambda: all(
                (self.ctx.db.get_sentinel_state(cls) or {}).get("last_checked_at")
                != prev_checked[cls]
                for cls in CTI_CLASSES
            ),
            timeout=15.0,
            description="all 21 CTI classes complete 1 steady-state sentinel cycle",
        )

        self.assertEqual(self.ctx.mock_nsac.count_boletim_requests(), 21)
        self.assertEqual(self.ctx.mock_fcm.count_messages(), 0)

        self.ctx.mock_nsac.release_class_grade(
            "INF3", "Programação Orientada a Objetos", 3, mt_grade="8.8", mb_grade="9.1"
        )
        self.ctx.db.trigger_sentinel_due_now("INF3")

        inf3_msgs = self.ctx.mock_fcm.wait_for_messages(
            5, timeout=12.0, only_succeeded=True
        )
        self.assertEqual({m.token for m in inf3_msgs}, set(tokens_by_class["INF3"]))
        for cls in CTI_CLASSES:
            if cls != "INF3":
                for tok in tokens_by_class[cls]:
                    self.assertEqual(self.ctx.mock_fcm.count_messages(token=tok), 0)

    def test_t4_cti_07_statistical_outlier_poisoning_resistance(self) -> None:
        def seed_outliers_and_cluster(db) -> None:  # type: ignore[no-untyped-def]
            # Class X (ELE2): 10 off-season outlier events ~90 days away.
            db.seed_grade_posting_events("ELE2", offset_days=-90, years_back=1, count=10)
            # Class Y (ELE3): 1 off-season outlier + 4 legitimate events inside +/-4d across 2 years.
            db.seed_grade_posting_events("ELE3", offset_days=-90, years_back=1, count=1)
            db.seed_grade_posting_events("ELE3", offset_days=-1, years_back=1, count=2)
            db.seed_grade_posting_events("ELE3", offset_days=-1, years_back=2, count=2)

        self.reset_isolated_state(seed_fn=seed_outliers_and_cluster)
        assert self.ctx.db is not None

        _, _, _, _, client_x, _ = self.create_and_login_noturno(
            "t4_cti_07_x", class_code="ELE2"
        )
        _, _, _, _, client_y, _ = self.create_and_login_noturno(
            "t4_cti_07_y", class_code="ELE3"
        )

        # Sample sentinel_class_state right after login (before get_boletim applies any API bump).
        state_x_before = self.ctx.db.get_sentinel_state("ELE2")
        state_y_before = self.ctx.db.get_sentinel_state("ELE3")
        self.assertIsNotNone(state_x_before)
        self.assertIsNotNone(state_y_before)
        assert state_x_before is not None and state_y_before is not None

        self.assertEqual(float(state_x_before["current_certainty"]), 0.0)
        self.assertEqual(
            int(state_x_before["current_interval_secs"]),
            self.ctx.config.sentinel_cold_baseline_secs,
        )
        self.assertGreater(float(state_y_before["current_certainty"]), 0.0)
        self.assertLess(
            int(state_y_before["current_interval_secs"]),
            self.ctx.config.sentinel_cold_baseline_secs,
        )

        resp_x = client_x.get_boletim()
        assert_boletim_envelope(resp_x, require_poll_hint=True)
        resp_y = client_y.get_boletim()
        assert_boletim_envelope(resp_y, require_poll_hint=True)

        state_x_after = self.ctx.db.get_sentinel_state("ELE2")
        state_y_after = self.ctx.db.get_sentinel_state("ELE3")
        self.assertIsNotNone(state_x_after)
        self.assertIsNotNone(state_y_after)
        assert state_x_after is not None and state_y_after is not None

        self.assertGreaterEqual(float(state_x_after["current_certainty"]), 0.0)
        self.assertLessEqual(float(state_x_after["current_certainty"]), 0.16)
        self.assertLess(
            float(state_x_after["current_certainty"]),
            float(state_y_after["current_certainty"]),
        )
        self.assertGreater(
            int(state_x_after["current_interval_secs"]),
            int(state_y_after["current_interval_secs"]),
        )
        self.assertLess(
            int(resp_y.json_body["next_poll_hint_secs"]),
            int(resp_x.json_body["next_poll_hint_secs"]),
        )


if __name__ == "__main__":
    unittest.main()
