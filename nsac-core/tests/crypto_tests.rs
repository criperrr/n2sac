use nsac_core::crypto::CryptoService;
use nsac_core::error::ErrorCode;
use nsac_core::models::{NsacCredentials, NsacSessionCookies};
use zeroize::Zeroizing;

const TEST_KEY_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER_KEY_HEX: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

#[test]
fn aes_gcm_round_trip_ascii_utf8_empty_and_long() {
    let crypto = CryptoService::from_hex_key(TEST_KEY_HEX).unwrap();

    let samples = [
        "",
        "simple-ascii-password",
        "Educação Física — Programação & Ação! çãõéíóú",
        &"XSRF-TOKEN=synthetic_value_12345; nsaconline_session=synthetic_sess_67890; ".repeat(10),
    ];

    for sample in samples {
        let encrypted = crypto.encrypt(sample).unwrap();
        let parts: Vec<&str> = encrypted.split(':').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "v1");
        assert_eq!(parts[1].len(), 24);
        assert!(!parts[2].is_empty());

        let decrypted = crypto.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted.as_str(), sample);
    }
}

#[test]
fn encrypt_generates_unique_nonce_per_call() {
    let crypto = CryptoService::from_hex_key(TEST_KEY_HEX).unwrap();
    let enc1 = crypto.encrypt("same-secret").unwrap();
    let enc2 = crypto.encrypt("same-secret").unwrap();

    assert_ne!(enc1, enc2);
    let nonce1 = enc1.split(':').nth(1).unwrap();
    let nonce2 = enc2.split(':').nth(1).unwrap();
    assert_ne!(nonce1, nonce2);

    assert_eq!(crypto.decrypt(&enc1).unwrap().as_str(), "same-secret");
    assert_eq!(crypto.decrypt(&enc2).unwrap().as_str(), "same-secret");
}

#[test]
fn decrypt_rejects_wrong_key_tampering_and_malformed_payloads() {
    let crypto = CryptoService::from_hex_key(TEST_KEY_HEX).unwrap();
    let wrong_crypto = CryptoService::from_hex_key(OTHER_KEY_HEX).unwrap();

    let encrypted = crypto.encrypt("sensitive-password").unwrap();

    // Wrong key
    let err = wrong_crypto.decrypt(&encrypted).unwrap_err();
    assert_eq!(err.code, ErrorCode::CryptoError);

    // Tampered ciphertext byte
    let mut parts: Vec<String> = encrypted.split(':').map(str::to_string).collect();
    let mut ct_bytes = hex::decode(&parts[2]).unwrap();
    ct_bytes[0] ^= 0xFF;
    parts[2] = hex::encode(ct_bytes);
    let tampered_ct = parts.join(":");
    assert_eq!(
        crypto.decrypt(&tampered_ct).unwrap_err().code,
        ErrorCode::CryptoError
    );

    // Tampered nonce byte
    let mut parts_nonce: Vec<String> = encrypted.split(':').map(str::to_string).collect();
    let mut nonce_bytes = hex::decode(&parts_nonce[1]).unwrap();
    nonce_bytes[0] ^= 0x01;
    parts_nonce[1] = hex::encode(nonce_bytes);
    let tampered_nonce = parts_nonce.join(":");
    assert_eq!(
        crypto.decrypt(&tampered_nonce).unwrap_err().code,
        ErrorCode::CryptoError
    );

    // Malformed payloads
    for bad in [
        "",
        "v2:000000000000000000000000:abcd",
        "v1:short:00112233445566778899aabbccddeeff",
        "v1:000000000000000000000000:nothex",
        "v1:000000000000000000000000:0011",
        "v1:000000000000000000000000:0011:extra",
    ] {
        let e = crypto.decrypt(bad).unwrap_err();
        assert_eq!(e.code, ErrorCode::CryptoError);
    }
}

#[test]
fn from_hex_key_rejects_invalid_keys() {
    for bad in [
        "",
        "012345",
        &"a".repeat(62),
        &"a".repeat(66),
        &"z".repeat(64),
    ] {
        let err = CryptoService::from_hex_key(bad).unwrap_err();
        assert_eq!(err.code, ErrorCode::CryptoError);
        assert!(!err.internal_message().contains(bad) || bad.is_empty());
    }
}

#[test]
fn debug_formatting_redacts_keys_passwords_and_cookies() {
    let crypto = CryptoService::from_hex_key(TEST_KEY_HEX).unwrap();
    let crypto_dbg = format!("{crypto:?}");
    assert!(!crypto_dbg.contains(TEST_KEY_HEX));
    assert!(crypto_dbg.contains("REDACTED"));

    let creds = NsacCredentials {
        email: "aluno@unesp.br".to_string(),
        password: Zeroizing::new("super-secret-pass-999".to_string()),
    };
    let creds_dbg = format!("{creds:?}");
    assert!(!creds_dbg.contains("super-secret-pass-999"));
    assert!(creds_dbg.contains("REDACTED"));

    let cookies = NsacSessionCookies::new(
        "synthetic_xsrf_secret_abc",
        "synthetic_session_secret_xyz",
        Some((
            "remember_web_deadbeef".to_string(),
            "synthetic_remember_secret_123".to_string(),
        )),
    )
    .unwrap();
    let cookies_dbg = format!("{cookies:?}");
    assert!(!cookies_dbg.contains("synthetic_xsrf_secret_abc"));
    assert!(!cookies_dbg.contains("synthetic_session_secret_xyz"));
    assert!(!cookies_dbg.contains("synthetic_remember_secret_123"));
    assert!(cookies_dbg.contains("REDACTED"));
    assert!(cookies_dbg.contains("has_remember_web: true"));
}

#[test]
fn app_error_serialization_and_display_hide_internal_details() {
    let err = nsac_core::error::AppError::nsac_unavailable("internal socket detail secret-xyz");
    assert_eq!(err.http_status(), 503);
    assert_eq!(err.internal_message(), "internal socket detail secret-xyz");

    let display = format!("{err}");
    assert!(!display.contains("secret-xyz"));
    assert!(display.contains("NSAC_UNAVAILABLE"));

    let json = serde_json::to_value(&err).unwrap();
    assert_eq!(json["code"], "NSAC_UNAVAILABLE");
    assert!(json["message"].as_str().unwrap().contains("NSAC"));
    assert!(json.get("internal_message").is_none());
    assert!(json.get("internalMessage").is_none());
}

