-- Allow custom formatted announcement tags (max 2 words, max 20 chars) instead of a fixed 5-value enum.

ALTER TABLE class_announcements
    DROP CONSTRAINT IF EXISTS class_announcements_category_check;

UPDATE class_announcements
SET category = CASE LOWER(BTRIM(category))
    WHEN 'general' THEN 'Geral'
    WHEN 'exam' THEN 'Prova'
    WHEN 'assignment' THEN 'Trabalho'
    WHEN 'schedule' THEN 'Horário'
    WHEN 'event' THEN 'Evento'
    ELSE LEFT(BTRIM(category), 20)
END;

ALTER TABLE class_announcements
    ALTER COLUMN category SET DEFAULT 'Geral';

ALTER TABLE class_announcements
    ADD CONSTRAINT class_announcements_category_check
    CHECK (char_length(btrim(category)) >= 1 AND char_length(category) <= 20);
