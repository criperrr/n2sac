-- Platform roles (user / mod / dev) and admin-assigned account tags.
-- Startup-only migration (ZERO runtime DDL).

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS platform_role TEXT NOT NULL DEFAULT 'user';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'accounts_platform_role_check'
    ) THEN
        ALTER TABLE accounts
            ADD CONSTRAINT accounts_platform_role_check
            CHECK (platform_role IN ('user', 'mod', 'dev'));
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS tag_definitions (
    slug        TEXT PRIMARY KEY,
    label       TEXT NOT NULL,
    description TEXT NOT NULL,
    color_hex   TEXT NOT NULL DEFAULT '#6366F1',
    priority    SMALLINT NOT NULL DEFAULT 50,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS account_tags (
    account_id  BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    tag_slug    TEXT NOT NULL REFERENCES tag_definitions(slug) ON DELETE CASCADE,
    assigned_by BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    reason      TEXT NOT NULL DEFAULT '',
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (account_id, tag_slug)
);

CREATE INDEX IF NOT EXISTS idx_account_tags_account_id
    ON account_tags (account_id);

INSERT INTO tag_definitions (slug, label, description, color_hex, priority)
VALUES
    ('dev',         'Dev',          'Desenvolvedor oficial da plataforma n²sac.',                 '#6366F1', 10),
    ('mod',         'Mod',          'Moderador oficial da comunidade n²sac.',                     '#EC4899', 15),
    ('bug_hunter',  'Bug Hunter',   'Ajudou a encontrar e corrigir bugs reais no n²sac.',         '#10B981', 30),
    ('contributor', 'Contribuidor', 'Contribuiu diretamente para a evolução da plataforma.',      '#0EA5E9', 35),
    ('monitor',     'Monitor',      'Monitor de disciplina que apoia colegas nos estudos.',       '#14B8A6', 40),
    ('gremio',      'Grêmio',       'Integrante do Grêmio Estudantil do CTI.',                    '#F97316', 45),
    ('pioneer',     'Pioneiro',     'Um dos primeiros alunos a testar o n²sac no lançamento.',    '#A855F7', 50)
ON CONFLICT (slug) DO NOTHING;

-- Bootstrap initial platform developer role/tag if account exists.
UPDATE accounts
SET platform_role = 'dev'
WHERE LOWER(nsac_email) = 'gabriel.camaforte@unesp.br'
  AND platform_role <> 'dev';

INSERT INTO account_tags (account_id, tag_slug, reason)
SELECT id, 'dev', 'Platform founder / lead developer'
FROM accounts
WHERE LOWER(nsac_email) = 'gabriel.camaforte@unesp.br'
ON CONFLICT (account_id, tag_slug) DO NOTHING;
