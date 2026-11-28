use sqlx::{FromRow, PgPool};

use crate::error::ServerError;

/// Exposure flags of one account (all `false` when it never answered anything).
#[derive(Debug, Clone, Copy, Default, FromRow)]
pub struct Visibility {
    pub opted_in: bool,
    pub hide_name: bool,
    pub show_avatar: bool,
    pub public_profile: bool,
    pub discoverable: bool,
    pub invisible: bool,
}

impl Visibility {
    /// Shows the real name in the ranking.
    pub fn named_in_ranking(&self) -> bool {
        self.opted_in && !self.hide_name
    }

    /// Can be found by name/e-mail search: named in the ranking, or opted into being found.
    /// Invisible mode overrides everything.
    pub fn findable(&self) -> bool {
        !self.invisible && (self.named_in_ranking() || self.discoverable)
    }

    /// Profile page open to any logged-in student: everyone whose name is visible in the ranking
    /// automatically has an accessible profile; discoverable students outside the ranking need
    /// `public_profile`. Invisible mode overrides everything.
    pub fn profile_visible(&self) -> bool {
        !self.invisible && (self.named_in_ranking() || (self.discoverable && self.public_profile))
    }

    /// Photo visible to non-friends: only when the student explicitly enabled `show_avatar`,
    /// is not invisible, and is not hiding their name. Accepted friends may still view the avatar.
    pub fn avatar_visible(&self) -> bool {
        !self.invisible
            && self.show_avatar
            && !self.hide_name
            && (self.named_in_ranking() || self.profile_visible())
    }
}

pub async fn visibility(pool: &PgPool, account_id: i64) -> Result<Visibility, ServerError> {
    let row = sqlx::query_as::<_, Visibility>(
        r#"
        SELECT opted_in, hide_name, show_avatar, public_profile, discoverable, invisible
        FROM account_ranking_preferences
        WHERE account_id = $1
        "#,
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.unwrap_or_default())
}

/// One findable student, as read for the search (filtered/ranked in Rust, accent-insensitive).
#[derive(FromRow)]
pub struct SearchCandidate {
    pub account_id: i64,
    pub nsac_email: String,
    pub class_code: Option<String>,
    pub ranking_display_name: Option<String>,
    pub opted_in: bool,
    pub hide_name: bool,
    pub show_avatar: bool,
    pub public_profile: bool,
    pub discoverable: bool,
    pub invisible: bool,
    pub avatar_version: Option<i64>,
    pub followers_count: i64,
    pub following_count: i64,
}

impl SearchCandidate {
    pub fn visibility(&self) -> Visibility {
        Visibility {
            opted_in: self.opted_in,
            hide_name: self.hide_name,
            show_avatar: self.show_avatar,
            public_profile: self.public_profile,
            discoverable: self.discoverable,
            invisible: self.invisible,
        }
    }
}

/// Every student that may appear in search (the CTI has ~630 students, so this stays small).
pub async fn search_candidates(pool: &PgPool) -> Result<Vec<SearchCandidate>, ServerError> {
    Ok(sqlx::query_as::<_, SearchCandidate>(
        r#"
        SELECT
            a.id AS account_id,
            a.nsac_email,
            a.class_code,
            COALESCE(NULLIF(TRIM(rp.display_name), ''), NULLIF(TRIM(ap.display_name), '')) AS ranking_display_name,
            rp.opted_in, rp.hide_name, rp.show_avatar, rp.public_profile, rp.discoverable, rp.invisible,
            CASE WHEN ap.avatar_data_url IS NOT NULL
                 THEN EXTRACT(EPOCH FROM ap.updated_at)::bigint END AS avatar_version,
            COALESCE((SELECT COUNT(*)::bigint FROM social_follows sf WHERE sf.followee_id = a.id AND sf.status = 'accepted'), 0) AS followers_count,
            COALESCE((SELECT COUNT(*)::bigint FROM social_follows sf WHERE sf.follower_id = a.id AND sf.status = 'accepted'), 0) AS following_count
        FROM accounts a
        JOIN account_ranking_preferences rp ON rp.account_id = a.id
        LEFT JOIN account_profiles ap ON ap.account_id = a.id
        WHERE a.active = TRUE
          AND rp.invisible = FALSE
          AND ((rp.opted_in AND NOT rp.hide_name) OR rp.discoverable)
        "#,
    )
    .fetch_all(pool)
    .await?)
}

/// Which stored image of a profile to read.
#[derive(Debug, Clone, Copy)]
pub enum ProfileImage {
    Avatar,
    Banner,
}

#[derive(FromRow)]
pub struct StoredImage {
    pub data_url: String,
    pub version: i64,
}

pub async fn image(pool: &PgPool, account_id: i64, kind: ProfileImage) -> Result<Option<StoredImage>, ServerError> {
    let sql = match kind {
        ProfileImage::Avatar => {
            "SELECT avatar_data_url AS data_url, EXTRACT(EPOCH FROM updated_at)::bigint AS version
             FROM account_profiles WHERE account_id = $1 AND avatar_data_url IS NOT NULL"
        }
        ProfileImage::Banner => {
            "SELECT banner_data_url AS data_url, EXTRACT(EPOCH FROM updated_at)::bigint AS version
             FROM account_profiles WHERE account_id = $1 AND banner_data_url IS NOT NULL"
        }
    };
    Ok(sqlx::query_as::<_, StoredImage>(sql)
        .bind(account_id)
        .fetch_optional(pool)
        .await?)
}

#[derive(FromRow)]
pub struct PublicProfileRow {
    pub nsac_email: String,
    pub verified_official_name: Option<String>,
    pub class_code: Option<String>,
    pub ranking_display_name: Option<String>,
    pub hide_name: Option<bool>,
    pub bio: Option<String>,
    pub has_avatar: bool,
    pub has_banner: bool,
    pub version: Option<i64>,
}

pub async fn public_profile(pool: &PgPool, account_id: i64) -> Result<Option<PublicProfileRow>, ServerError> {
    Ok(sqlx::query_as::<_, PublicProfileRow>(
        r#"
        SELECT
            a.nsac_email,
            a.verified_official_name,
            a.class_code,
            COALESCE(NULLIF(TRIM(rp.display_name), ''), NULLIF(TRIM(ap.display_name), '')) AS ranking_display_name,
            rp.hide_name,
            ap.bio,
            (ap.avatar_data_url IS NOT NULL) AS has_avatar,
            (ap.banner_data_url IS NOT NULL) AS has_banner,
            EXTRACT(EPOCH FROM ap.updated_at)::bigint AS version
        FROM accounts a
        LEFT JOIN account_ranking_preferences rp ON rp.account_id = a.id
        LEFT JOIN account_profiles ap ON ap.account_id = a.id
        WHERE a.id = $1 AND a.active = TRUE
        "#,
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?)
}

/// Splits `data:<mime>;base64,<payload>` into its content type and decoded bytes.
pub fn decode_data_url(data_url: &str) -> Option<(String, Vec<u8>)> {
    use base64::Engine as _;
    let rest = data_url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    if !mime.starts_with("image/") || mime.contains(['\r', '\n']) {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(payload.trim()).ok()?;
    Some((mime.to_string(), bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMED: Visibility = Visibility {
        opted_in: true,
        hide_name: false,
        show_avatar: false,
        public_profile: false,
        discoverable: false,
        invisible: false,
    };

    #[test]
    fn findable_by_ranking_name_or_explicit_opt_in_never_when_invisible() {
        assert!(NAMED.findable());
        assert!(!Visibility::default().findable(), "never answered anything");
        assert!(!Visibility { hide_name: true, ..NAMED }.findable(), "anonymous in the ranking");
        assert!(Visibility { discoverable: true, ..Visibility::default() }.findable(), "opted into search");
        assert!(Visibility { hide_name: true, discoverable: true, ..NAMED }.findable());
        assert!(!Visibility { invisible: true, discoverable: true, ..NAMED }.findable(), "invisible wins");
    }

    #[test]
    fn profile_accessible_when_named_in_ranking_or_discoverable_public() {
        assert!(NAMED.profile_visible(), "named in the ranking always has an accessible profile");
        assert!(Visibility { public_profile: true, ..NAMED }.profile_visible());
        assert!(!Visibility { public_profile: true, hide_name: true, ..NAMED }.profile_visible());
        assert!(Visibility { public_profile: true, discoverable: true, ..Visibility::default() }.profile_visible());
        assert!(!Visibility { discoverable: true, ..Visibility::default() }.profile_visible());
        assert!(!Visibility { public_profile: true, invisible: true, ..NAMED }.profile_visible());
    }

    #[test]
    fn avatar_follows_ranking_or_profile_visibility() {
        let shown = Visibility { show_avatar: true, ..NAMED };
        assert!(shown.avatar_visible());
        assert!(!Visibility { hide_name: true, ..shown }.avatar_visible());
        assert!(!NAMED.avatar_visible(), "hidden for non-friends when show_avatar is false");
        assert!(!Visibility { discoverable: true, ..Visibility::default() }.avatar_visible());
        assert!(
            !Visibility { discoverable: true, public_profile: true, ..Visibility::default() }
                .avatar_visible(),
            "show_avatar=false hides avatar even on public profile for non-friends"
        );
        assert!(
            Visibility {
                discoverable: true,
                public_profile: true,
                show_avatar: true,
                ..Visibility::default()
            }
            .avatar_visible()
        );
        assert!(!Visibility { invisible: true, ..shown }.avatar_visible());
    }

    #[test]
    fn decodes_image_data_urls_only() {
        let (mime, bytes) = decode_data_url("data:image/jpeg;base64,/9j/4A==").unwrap();
        assert_eq!(mime, "image/jpeg");
        assert_eq!(bytes, vec![0xff, 0xd8, 0xff, 0xe0]);
        assert!(decode_data_url("data:text/html;base64,PGI+").is_none());
        assert!(decode_data_url("https://example.com/a.png").is_none());
        assert!(decode_data_url("data:image/png,raw").is_none());
    }
}
