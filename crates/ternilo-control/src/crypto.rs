use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    Key, KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rand::random;
use sha2::{Digest, Sha256};
use ternilo_protocol::HarnessError;
use zeroize::Zeroizing;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncryptedSecret {
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

pub struct SecretCipher {
    key: Zeroizing<[u8; 32]>,
}

impl SecretCipher {
    pub fn from_base64(encoded: &str) -> Result<Self, HarnessError> {
        let decoded = Zeroizing::new(STANDARD.decode(encoded.trim()).map_err(|error| {
            HarnessError::invalid(format!("decode secret master key as base64: {error}"))
        })?);
        let key: [u8; 32] = decoded.as_slice().try_into().map_err(|_| {
            HarnessError::invalid("secret master key must decode to exactly 32 bytes")
        })?;
        Ok(Self {
            key: Zeroizing::new(key),
        })
    }

    #[must_use]
    pub fn from_key(key: [u8; 32]) -> Self {
        Self {
            key: Zeroizing::new(key),
        }
    }

    pub fn encrypt(
        &self,
        tenant_id: &str,
        project_id: Option<&str>,
        name: &str,
        version: u64,
        plaintext: &[u8],
    ) -> Result<EncryptedSecret, HarnessError> {
        let nonce = random::<[u8; 24]>();
        let cipher = self.cipher();
        let ciphertext = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext,
                    aad: &secret_aad(tenant_id, project_id, name, version),
                },
            )
            .map_err(|_| HarnessError::execution("encrypt tenant secret"))?;
        Ok(EncryptedSecret { nonce, ciphertext })
    }

    pub fn decrypt(
        &self,
        tenant_id: &str,
        project_id: Option<&str>,
        name: &str,
        version: u64,
        encrypted: &EncryptedSecret,
    ) -> Result<Zeroizing<Vec<u8>>, HarnessError> {
        let plaintext = self
            .cipher()
            .decrypt(
                &XNonce::from(encrypted.nonce),
                Payload {
                    msg: &encrypted.ciphertext,
                    aad: &secret_aad(tenant_id, project_id, name, version),
                },
            )
            .map_err(|_| HarnessError::policy("tenant secret authentication failed"))?;
        Ok(Zeroizing::new(plaintext))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&Key::from(*self.key))
    }
}

#[must_use]
pub fn random_identifier(prefix: &str) -> String {
    let random = random::<[u8; 16]>();
    format!(
        "{prefix}_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
    )
}

#[must_use]
pub fn random_token(prefix: &str) -> String {
    let random = random::<[u8; 32]>();
    format!(
        "{prefix}_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
    )
}

#[must_use]
pub fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

#[must_use]
pub fn chained_hash(previous: Option<&[u8]>, payload: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ternilo-audit-v1\0");
    if let Some(previous) = previous {
        digest.update(previous);
    }
    digest.update(payload);
    digest.finalize().into()
}

#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn secret_aad(tenant_id: &str, project_id: Option<&str>, name: &str, version: u64) -> Vec<u8> {
    let project_id = project_id.unwrap_or("<tenant>");
    format!("ternilo-secret-v1\0{tenant_id}\0{project_id}\0{name}\0{version}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_cipher_binds_ciphertext_to_tenant_name_and_version() {
        let cipher = SecretCipher::from_key([7; 32]);
        let encrypted = cipher
            .encrypt("tenant-a", Some("project-a"), "api-key", 1, b"secret")
            .unwrap();
        assert_eq!(
            cipher
                .decrypt("tenant-a", Some("project-a"), "api-key", 1, &encrypted)
                .unwrap()
                .as_slice(),
            b"secret"
        );
        assert!(
            cipher
                .decrypt("tenant-b", Some("project-a"), "api-key", 1, &encrypted)
                .is_err()
        );
        assert!(
            cipher
                .decrypt("tenant-a", Some("project-a"), "api-key", 2, &encrypted)
                .is_err()
        );
        assert!(
            cipher
                .decrypt("tenant-a", Some("project-b"), "api-key", 1, &encrypted)
                .is_err()
        );
    }

    #[test]
    fn audit_hash_changes_with_previous_entry() {
        let first = chained_hash(None, b"entry");
        let second = chained_hash(Some(&first), b"entry");
        assert_ne!(first, second);
    }
}
