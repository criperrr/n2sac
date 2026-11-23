-- Synthetic test users for the local n²sac environment (never production data).
-- Sessions use fixed tokens: nsac_usr_ + 64 × the user's letter (see users.json).

TRUNCATE accounts RESTART IDENTITY CASCADE;

INSERT INTO accounts (id, nsac_email, nsac_pass_enc, class_code) VALUES
  (1, 'voce.teste@aluno.example',   'x', 'INI3'),
  (2, 'bruno.lima@aluno.example',   'x', 'INI3'),
  (3, 'carla.mendes@aluno.example', 'x', 'INI3'),
  (4, 'diego.souza@aluno.example',  'x', 'INI3'),
  (5, 'elisa.rocha@aluno.example',  'x', 'INI3'),
  (6, 'felipe.costa@aluno.example', 'x', 'INI3');
SELECT setval('accounts_id_seq', 6);

INSERT INTO user_sessions (account_id, token_hash, expires_at)
SELECT id, encode(sha256(convert_to('nsac_usr_' || repeat(letter, 64), 'UTF8')), 'hex'), NOW() + INTERVAL '365 days'
FROM (VALUES (1, 'a'), (2, 'b'), (3, 'c'), (4, 'd'), (5, 'e'), (6, 'f')) AS t(id, letter);

-- Everyone already decided on the ranking (no consent popup) and shows their name.
INSERT INTO account_ranking_preferences (account_id, opted_in, hide_name, has_decided, public_profile, discoverable)
SELECT id, TRUE, FALSE, TRUE, TRUE, TRUE FROM accounts;

INSERT INTO account_profiles (account_id, display_name, bio) VALUES
  (2, NULL, 'Robô de teste: respondo, reajo e mando GIF 🤖'),
  (3, NULL, 'Robô de teste: digito devagar ⌨️'),
  (4, NULL, 'Robô de teste: só reajo às mensagens');

-- Friends (mutual follows): Bruno, Carla, Diego.
INSERT INTO social_follows (follower_id, followee_id, status) VALUES
  (1, 2, 'accepted'), (2, 1, 'accepted'),
  (1, 3, 'accepted'), (3, 1, 'accepted'),
  (1, 4, 'accepted'), (4, 1, 'accepted'),
  -- Elisa asked to follow you (pending request); Felipe follows you (not a friend yet).
  (5, 1, 'pending'),
  (6, 1, 'accepted');

-- ============================================================================
-- Realistic multi-bimester CTI INI3 boletim (so Boletim, Grade Modals, Charts,
-- and Ranking/Peer Comparison work richly out of the box on the test server).
-- ============================================================================
TRUNCATE school_years RESTART IDENTITY CASCADE;

INSERT INTO school_years (id, year, title) VALUES
  (1, 2025, '2º Ano - Informática (INI2)'),
  (2, 2026, '3º Ano - Informática (INI3)');
SELECT setval('school_years_id_seq', 2);

INSERT INTO accounts_school_years (account_id, school_year_id, status)
SELECT a.id, sy.id, CASE WHEN sy.year = 2025 THEN 'Aprovado' ELSE 'Em Curso' END
FROM accounts a
CROSS JOIN school_years sy;

INSERT INTO subjects (id, school_year_id, abbreviation, name) VALUES
  -- 2025 (INI2)
  (1, 1, 'FIS', 'Física'),
  (2, 1, 'MAT', 'Matemática'),
  (3, 1, 'LP',  'Língua Portuguesa'),
  (4, 1, 'QUI', 'Química'),
  -- 2026 (INI3)
  (5,  2, 'FIS', 'Física'),
  (6,  2, 'MAT', 'Matemática'),
  (7,  2, 'PWB', 'Programação Web'),
  (8,  2, 'BD',  'Banco de Dados'),
  (9,  2, 'QUI', 'Química'),
  (10, 2, 'LP',  'Língua Portuguesa'),
  (11, 2, 'SO',  'Sistemas Operacionais'),
  (12, 2, 'ING', 'Língua Inglesa');
SELECT setval('subjects_id_seq', 12);

-- 2025 full year (4 bimesters) for all 6 students
INSERT INTO grades (account_id, subject_id, bimester, grade, average_grade, absences, recovery_code, recovery_message, approved, recovered)
SELECT
  a.id,
  s.id,
  b.bim,
  ROUND(LEAST(10.0, GREATEST(4.5, (base.g + ((a.id - 1) * 0.25) - (CASE WHEN a.id IN (4, 6) THEN 1.1 ELSE 0 END) + (b.bim * 0.15))))::numeric, 1)::double precision,
  base.avg,
  (b.bim % 2),
  'NAC',
  'Não Aconteceu',
  TRUE,
  FALSE
FROM accounts a
CROSS JOIN (VALUES
  (1, 7.6, 6.9),
  (2, 8.0, 7.1),
  (3, 8.2, 7.5),
  (4, 7.2, 6.8)
) AS base(subject_id, g, avg)
JOIN subjects s ON s.id = base.subject_id
CROSS JOIN (VALUES (1), (2), (3), (4)) AS b(bim);

-- 2026 INI3 (1º, 2º, and newly posted 3º Bimestre with realistic evolution for voce!)
INSERT INTO grades (account_id, subject_id, bimester, grade, average_grade, absences, recovery_code, recovery_message, approved, recovered)
VALUES
  -- voce (account 1) - Física (evolution: 7.0 -> 7.8 -> 8.8!)
  (1, 5, 1, 7.0, 6.6, 2, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 5, 2, 7.8, 6.9, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 5, 3, 8.8, 7.1, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  -- voce - Matemática (evolution: 8.2 -> 8.5 -> 9.2!)
  (1, 6, 1, 8.2, 7.0, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 6, 2, 8.5, 7.2, 2, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 6, 3, 9.2, 7.4, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  -- voce - Programação Web (9.0 -> 9.4 -> 10.0!)
  (1, 7, 1, 9.0, 7.8, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 7, 2, 9.4, 8.0, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 7, 3, 10.0, 8.1, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  -- voce - Banco de Dados (8.0 -> 8.6 -> 9.0)
  (1, 8, 1, 8.0, 7.3, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 8, 2, 8.6, 7.5, 1, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 8, 3, 9.0, 7.6, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  -- voce - Química (5.5 rec -> 6.8 -> 7.6)
  (1, 9, 1, 5.5, 6.4, 4, 'SAT', 'Satisfatório', FALSE, TRUE),
  (1, 9, 2, 6.8, 6.7, 2, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 9, 3, 7.6, 6.9, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  -- voce - Língua Portuguesa (8.0 -> 8.2 -> 8.4)
  (1, 10, 1, 8.0, 7.6, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 10, 2, 8.2, 7.7, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 10, 3, 8.4, 7.8, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  -- voce - Sistemas Operacionais (8.5 -> 8.9 -> 9.3)
  (1, 11, 1, 8.5, 7.4, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 11, 2, 8.9, 7.6, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 11, 3, 9.3, 7.7, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  -- voce - Língua Inglesa (9.0 -> 9.2 -> 10.0!)
  (1, 12, 1, 9.0, 8.1, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 12, 2, 9.2, 8.2, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE),
  (1, 12, 3, 10.0, 8.3, 0, 'NAC', 'Não Aconteceu', TRUE, FALSE);

-- Classmates (accounts 2..6) 2026 grades so Ranking & Peer Comparison look realistic
INSERT INTO grades (account_id, subject_id, bimester, grade, average_grade, absences, recovery_code, recovery_message, approved, recovered)
SELECT
  a.id,
  g.subject_id,
  g.bimester,
  ROUND(LEAST(10.0, GREATEST(4.0, g.grade + delta.grade_delta))::numeric, 1)::double precision,
  g.average_grade,
  GREATEST(0, g.absences + (a.id % 3)),
  'NAC',
  'Não Aconteceu',
  TRUE,
  FALSE
FROM grades g
JOIN (VALUES
  (2,  0.3),  -- Bruno (slightly ahead -> #1)
  (3, -0.2),  -- Carla (#3)
  (4, -0.6),  -- Diego (#4)
  (5,  0.1),  -- Elisa (#2)
  (6, -1.0)   -- Felipe (#6)
) AS delta(account_id, grade_delta) ON TRUE
JOIN accounts a ON a.id = delta.account_id
WHERE g.account_id = 1 AND g.subject_id >= 5;

-- Subject final results (2025 finalized, 2026 in progress with running absences)
INSERT INTO subject_final_results (account_id, subject_id, final_grade, total_absences)
SELECT
  g.account_id,
  g.subject_id,
  CASE WHEN s.school_year_id = 1 THEN ROUND(AVG(g.grade)::numeric, 1)::double precision ELSE NULL END,
  SUM(g.absences)::int
FROM grades g
JOIN subjects s ON s.id = g.subject_id
GROUP BY g.account_id, g.subject_id, s.school_year_id;

-- Bimester summary metrics per account & school year
INSERT INTO bimesters (account_id, school_year_id, bimester, user_average, class_average, total_absences)
SELECT
  g.account_id,
  s.school_year_id,
  g.bimester,
  ROUND(AVG(g.grade)::numeric, 2)::double precision,
  ROUND(AVG(g.average_grade)::numeric, 2)::double precision,
  SUM(g.absences)::int
FROM grades g
JOIN subjects s ON s.id = g.subject_id
GROUP BY g.account_id, s.school_year_id, g.bimester;

-- Grade posting history events for Sentinel/Alerts telemetry
INSERT INTO grade_posting_events (class_code, school_year, bimester, subject_name, day_of_year, month, day, posted_at) VALUES
  ('INI3', 2026, 3, 'Física', 327, 11, 23, NOW() - INTERVAL '18 minutes'),
  ('INI3', 2026, 3, 'Matemática', 327, 11, 23, NOW() - INTERVAL '42 minutes'),
  ('INI3', 2026, 3, 'Programação Web', 326, 11, 22, NOW() - INTERVAL '1 day'),
  ('INI3', 2026, 2, 'Física', 240, 8, 28, NOW() - INTERVAL '85 days');
