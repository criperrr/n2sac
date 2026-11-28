-- Ranking query covering indexes and social/representative fast lookup indexes.
-- Startup-only migration (ZERO runtime DDL).

CREATE INDEX IF NOT EXISTS idx_grades_subject_posted_covering
    ON grades (subject_id, account_id, bimester)
    INCLUDE (grade, average_grade, absences, recovery_code, recovered, approved)
    WHERE grade IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_ranking_prefs_eligible_account
    ON account_ranking_preferences (account_id)
    INCLUDE (hide_name, display_name, show_avatar, public_profile)
    WHERE opted_in = TRUE AND invisible = FALSE;

CREATE INDEX IF NOT EXISTS idx_class_representatives_account_room
    ON class_representatives (account_id, room_id);

CREATE INDEX IF NOT EXISTS idx_social_follows_accepted_pair
    ON social_follows (follower_id, followee_id)
    WHERE status = 'accepted';
