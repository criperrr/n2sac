use std::fmt;

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use zeroize::Zeroizing;

use crate::error::AppError;

pub type SecretString = Zeroizing<String>;

const PAYLOAD_VERSION: &str = "v1";
const NONCE_BYTES: usize = 12;
const NONCE_HEX_LEN: usize = NONCE_BYTES * 2;
const GCM_TAG_BYTES: usize = 16;

#[derive(Clone)]
pub struct CryptoService {
    key: Zeroizing<[u8; 32]>,
}

impl fmt::Debug for CryptoService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CryptoService")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl CryptoService {
    pub fn from_hex_key(hex_key: &str) -> Result<Self, AppError> {
        let trimmed = hex_key.trim();
        if trimmed.len() != 64 {
            return Err(AppError::crypto(
                "Invalid encryption key length: expected 64 hex characters",
            ));
        }

        let mut key = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(trimmed, &mut *key)
            .map_err(|_| AppError::crypto("Invalid hex encryption key"))?;

        Ok(Self { key })
    }

    pub fn from_bytes(key: [u8; 32]) -> Self {
        Self {
            key: Zeroizing::new(key),
        }
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<String, AppError> {
        let cipher = Aes256Gcm::new_from_slice(&*self.key)
            .map_err(|_| AppError::crypto("Failed to initialize AES-256-GCM cipher"))?;

        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|_| AppError::crypto("AES-256-GCM encryption failed"))?;

        Ok(format!(
            "{}:{}:{}",
            PAYLOAD_VERSION,
            hex::encode(nonce),
            hex::encode(ciphertext)
        ))
    }

    pub fn decrypt(&self, payload: &str) -> Result<SecretString, AppError> {
        let mut parts = payload.split(':');
        let Some(version) = parts.next() else {
            return Err(AppError::crypto("Missing encrypted payload version"));
        };
        let Some(nonce_hex) = parts.next() else {
            return Err(AppError::crypto("Missing encrypted payload nonce"));
        };
        let Some(ct_hex) = parts.next() else {
            return Err(AppError::crypto("Missing encrypted payload ciphertext"));
        };
        if parts.next().is_some() {
            return Err(AppError::crypto("Unexpected extra segments in payload"));
        }

        if version != PAYLOAD_VERSION {
            return Err(AppError::crypto("Unsupported encrypted payload version"));
        }

        if nonce_hex.len() != NONCE_HEX_LEN {
            return Err(AppError::crypto("Invalid encrypted payload nonce length"));
        }

        let mut nonce_bytes = [0u8; NONCE_BYTES];
        hex::decode_to_slice(nonce_hex, &mut nonce_bytes)
            .map_err(|_| AppError::crypto("Invalid encrypted payload nonce hex"))?;

        let ciphertext = Zeroizing::new(
            hex::decode(ct_hex)
                .map_err(|_| AppError::crypto("Invalid encrypted payload ciphertext hex"))?,
        );
        if ciphertext.len() < GCM_TAG_BYTES {
            return Err(AppError::crypto("Encrypted payload ciphertext too short"));
        }

        let cipher = Aes256Gcm::new_from_slice(&*self.key)
            .map_err(|_| AppError::crypto("Failed to initialize AES-256-GCM cipher"))?;

        let decrypted = Zeroizing::new(
            cipher
                .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
                .map_err(|_| AppError::crypto("Decryption failed or authentication tag mismatch"))?,
        );

        let plaintext = std::str::from_utf8(&decrypted)
            .map_err(|_| AppError::crypto("Decrypted payload is not valid UTF-8"))?;

        Ok(Zeroizing::new(plaintext.to_owned()))
    }
}
