mod common;

use chrono::Utc;
use nsac_core::crypto::CryptoService;
use nsac_core::models::NsacSessionCookies;
use nsac_server::db::social::MessageQuery;
use nsac_server::db::{AccountRepo, SocialRepo};
use sqlx::PgPool;

fn test_crypto() -> CryptoService {
    CryptoService::from_hex_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        .unwrap()
}

async fn account(pool: &PgPool, crypto: &CryptoService, name: &str) -> i64 {
    let cookies = NsacSessionCookies::new("x", "s", None).unwrap();
    let email = format!("{name}.{}@chat.test", common::unique_suffix());
    AccountRepo::new(pool.clone())
        .upsert_on_login(crypto, &email, "pw", &cookies, Some("31A"), None, None, Utc::now())
        .await
        .unwrap()
        .id
}

async fn befriend(pool: &PgPool, a: i64, b: i64) {
    for (f, t) in [(a, b), (b, a)] {
        sqlx::query("INSERT INTO social_follows (follower_id, followee_id, status) VALUES ($1, $2, 'accepted')")
            .bind(f)
            .bind(t)
            .execute(pool)
            .await
            .unwrap();
    }
}

fn latest() -> MessageQuery {
    MessageQuery { limit: 60, ..MessageQuery::default() }
}

#[tokio::test]
async fn replies_reactions_edits_unsend_receipts_and_rev_sync() {
    let pool = common::test_pool().await;
    let crypto = test_crypto();
    let social = SocialRepo::new(pool.clone());
    let a = account(&pool, &crypto, "ana").await;
    let b = account(&pool, &crypto, "bia").await;
    befriend(&pool, a, b).await;

    let first = social.send_message(a, b, "oi bia", None, &crypto).await.unwrap().message;
    let reply = social
        .send_message(b, a, "oi ana!", Some(first.id), &crypto)
        .await
        .unwrap()
        .message;
    let preview = reply.reply_to.expect("reply preview");
    assert_eq!(preview.id, first.id);
    assert_eq!(preview.text, "oi bia");
    assert!(!preview.is_me, "B sees A's message as not theirs");

    // B loaded the conversation when replying, so A sees a read receipt up to the reply.
    let view_a = social.get_messages(a, b, latest(), &crypto).await.unwrap();
    assert_eq!(view_a.messages.len(), 2);
    assert_eq!(view_a.peer_last_read_id, reply.id);
    assert!(!view_a.has_more_before);
    let cursor = view_a.next_rev;

    // Reaction from A on B's reply shows up in B's rev poll as a changed message.
    let reacted = social.set_reaction(a, b, reply.id, Some("❤️"), &crypto).await.unwrap();
    assert_eq!(reacted.reactions.len(), 1);
    assert!(reacted.reactions[0].mine);
    assert!(social.set_reaction(a, b, reply.id, Some("🍕"), &crypto).await.is_err());

    let poll = social
        .get_messages(a, b, MessageQuery { after_rev: Some(cursor), limit: 50, ..MessageQuery::default() }, &crypto)
        .await
        .unwrap();
    assert_eq!(poll.messages.len(), 1);
    assert_eq!(poll.messages[0].id, reply.id);
    assert!(poll.next_rev > cursor);

    // Edit: only own messages.
    assert!(social.edit_message(b, a, first.id, "hackeado", &crypto).await.is_err());
    let edited = social.edit_message(a, b, first.id, "oi bia, tudo bem?", &crypto).await.unwrap();
    assert!(edited.edited_at.is_some());
    assert_eq!(edited.text, "oi bia, tudo bem?");

    // Unsend: tombstone for both, reactions gone, reply preview marked deleted.
    assert!(social.unsend_message(a, b, reply.id, &crypto).await.is_err());
    let gone = social.unsend_message(a, b, first.id, &crypto).await.unwrap();
    assert!(gone.deleted && gone.text.is_empty());
    let view_b = social.get_messages(b, a, latest(), &crypto).await.unwrap();
    let tomb = view_b.messages.iter().find(|m| m.id == first.id).unwrap();
    assert!(tomb.deleted && tomb.text.is_empty());
    let r = view_b.messages.iter().find(|m| m.id == reply.id).unwrap();
    assert!(r.reply_to.as_ref().unwrap().deleted);
    assert!(social.send_message(b, a, "x", Some(first.id), &crypto).await.is_err());

    // Older page.
    for i in 0..5 {
        social.send_message(a, b, &format!("m{i}"), None, &crypto).await.unwrap();
    }
    let page = social
        .get_messages(a, b, MessageQuery { limit: 3, ..MessageQuery::default() }, &crypto)
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 3);
    assert!(page.has_more_before);
    let older = social
        .get_messages(
            a,
            b,
            MessageQuery { before_id: Some(page.messages[0].id), limit: 50, ..MessageQuery::default() },
            &crypto,
        )
        .await
        .unwrap();
    assert_eq!(older.messages.len(), 4);
    assert!(!older.has_more_before);
}
