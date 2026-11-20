"""Tier 1 — Area D: Boletim query filters (PF-01, PF-02, PF-03, PF-EXTRA-01/03)."""

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
    bimester_number,
    bimester_personal,
    class_average_grade,
    iter_boletim_cells,
    personal_grade,
    personal_recovery,
    personal_recovery_code,
    subject_name,
    year_grades,
    year_number,
)

from tests._helpers import OpaqueE2ETestCase, assert_boletim_envelope


class TestTier1Filters(OpaqueE2ETestCase):
    def test_t1_filt_01_pf01_class_average_gt_reads_class_average_not_personal(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t1_filt_01", class_code="INF1")

        resp = client.get_boletim(filters={"classAverage[gt]": "7.0"})
        payload, years = assert_boletim_envelope(resp)
        cells = list(iter_boletim_cells(payload))
        self.assertGreater(len(cells), 0)

        matched_pairs = set()
        for _, subj, bim_num, bim in cells:
            avg = class_average_grade(bimester_class(bim))
            self.assertIsNotNone(avg)
            assert avg is not None
            self.assertGreater(avg, 7.0)
            matched_pairs.add((subj, bim_num))

        # Matematica B1 has class average 7.8 (> 7.0), Historia B1 has personal 7.5 but class 7.0 (not > 7.0).
        self.assertIn(("Matemática", 1), matched_pairs)
        self.assertNotIn(("História", 1), matched_pairs)
        self.assertEqual(len(years), 1)

    def test_t1_filt_02_pf03_accent_insensitive_and_case_insensitive_subject_filter(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t1_filt_02", class_code="INF1")

        for query, expected_subject in (
            ({"subjectName[contains]": "lingua"}, "Língua Portuguesa"),
            ({"subjectName[contains]": "programacao"}, "Programação Orientada a Objetos"),
            ({"subjectName[eq]": "matematica"}, "Matemática"),
        ):
            resp = client.get_boletim(filters=query)
            _, years = assert_boletim_envelope(resp)
            self.assertEqual(len(years), 1)
            subjects = [subject_name(s) for s in year_grades(years[0])]
            self.assertEqual(subjects, [expected_subject])

    def test_t1_filt_03_pf02_sequential_filters_do_not_mutate_cached_boletim(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t1_filt_03", class_code="INF2")

        r1 = client.get_boletim(filters={"targetBimester": "1"})
        p1, _ = assert_boletim_envelope(r1)
        self.assertTrue(all(b == 1 for _, _, b, _ in iter_boletim_cells(p1)))

        r2 = client.get_boletim(filters={"targetBimester": "2"})
        p2, _ = assert_boletim_envelope(r2)
        self.assertTrue(all(b == 2 for _, _, b, _ in iter_boletim_cells(p2)))

        r_full = client.get_boletim()
        p_full, years_full = assert_boletim_envelope(r_full)
        self.assertEqual(len(year_grades(years_full[0])), 7)
        self.assertEqual(len(list(iter_boletim_cells(p_full))), 28)

    def test_t1_filt_04_school_year_and_is_recovery_eq_neq(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_diurno("t1_filt_04", class_code="INI3A")

        resp_rec = client.get_boletim(
            filters={"schoolYear": "2025", "isRecovery[eq]": "true"}
        )
        p_rec, years_rec = assert_boletim_envelope(resp_rec)
        self.assertEqual([year_number(y) for y in years_rec], [2025])
        cells_rec = list(iter_boletim_cells(p_rec))
        self.assertGreater(len(cells_rec), 0)
        for yr, _, _, bim in cells_rec:
            self.assertEqual(yr, 2025)
            self.assertTrue(personal_recovery(bimester_personal(bim)))

        resp_non = client.get_boletim(
            filters={"schoolYear": "2025", "isRecovery[neq]": "true"}
        )
        p_non, years_non = assert_boletim_envelope(resp_non)
        self.assertEqual([year_number(y) for y in years_non], [2025])
        cells_non = list(iter_boletim_cells(p_non))
        self.assertGreater(len(cells_non), 0)
        for yr, _, _, bim in cells_non:
            self.assertEqual(yr, 2025)
            self.assertFalse(personal_recovery(bimester_personal(bim)))

    def test_t1_filt_05_range_and_comma_or_list_intersection(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_noturno("t1_filt_05", class_code="INF1")

        resp_range = client.get_boletim(filters={"grade[gte]": "8.0", "grade[lte]": "9.0"})
        p_range, _ = assert_boletim_envelope(resp_range)
        range_cells = list(iter_boletim_cells(p_range))
        self.assertGreater(len(range_cells), 0)
        for _, _, _, bim in range_cells:
            g = personal_grade(bimester_personal(bim))
            self.assertIsNotNone(g)
            assert g is not None
            self.assertGreaterEqual(g, 8.0)
            self.assertLessEqual(g, 9.0)

        resp_or = client.get_boletim(
            filters={"targetBimester": "1,3", "subjectName[startsWith]": "Mat,Fis"}
        )
        p_or, _ = assert_boletim_envelope(resp_or)
        or_cells = list(iter_boletim_cells(p_or))
        self.assertEqual(len(or_cells), 4)
        for _, subj, bim_num, bim in or_cells:
            self.assertIn(subj, {"Matemática", "Física"})
            self.assertIn(bim_num, {1, 3})
            self.assertEqual(bimester_number(bim), bim_num)

    def test_t1_filt_06_recovery_code_and_snake_case_aliases(self) -> None:
        _, _, _, _, client, _ = self.create_and_login_diurno("t1_filt_06", class_code="INI3A")

        camel_resp = client.get_boletim(
            filters={
                "schoolYear": "2026",
                "recoveryCode": "SAT,INS",
                "isRecovery": "true",
            }
        )
        snake_resp = client.get_boletim(
            filters={
                "school_year": "2026",
                "recovery_code": "SAT,INS",
                "is_recovery": "true",
            }
        )
        p_camel, _ = assert_boletim_envelope(camel_resp)
        p_snake, _ = assert_boletim_envelope(snake_resp)

        camel_cells = [
            (yr, subj, bim_num, personal_recovery_code(bimester_personal(bim)))
            for yr, subj, bim_num, bim in iter_boletim_cells(p_camel)
        ]
        snake_cells = [
            (yr, subj, bim_num, personal_recovery_code(bimester_personal(bim)))
            for yr, subj, bim_num, bim in iter_boletim_cells(p_snake)
        ]
        self.assertGreater(len(camel_cells), 0)
        self.assertEqual(camel_cells, snake_cells)
        for _, _, _, code in camel_cells:
            self.assertIn(code, {"SAT", "INS"})


if __name__ == "__main__":
    unittest.main()
