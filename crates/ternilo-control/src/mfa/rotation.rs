use super::SCOPE;
use crate::EncryptedSecret;
use crate::SecretCipher;
use sqlx::Row;
use ternilo_protocol::HarnessError;
use ternilo_storage::{Transaction, database_error};

pub(crate) async fn rotate(
    tx: &mut Transaction,
    current: &SecretCipher,
    next: &SecretCipher,
) -> Result<u64, HarnessError> {
    let mut count = 0;
    for (select, update) in [
        (
            "SELECT user_id AS cursor,user_id,generation AS context,nonce,ciphertext FROM control_mfa_factors WHERE user_id>$1 ORDER BY user_id LIMIT 128",
            "UPDATE control_mfa_factors SET nonce=$2,ciphertext=$3 WHERE user_id=$1",
        ),
        (
            "SELECT token_hash AS cursor,user_id,token_hash AS context,nonce,ciphertext FROM control_mfa_oidc_challenges WHERE token_hash>$1 ORDER BY token_hash LIMIT 128",
            "UPDATE control_mfa_oidc_challenges SET nonce=$2,ciphertext=$3 WHERE token_hash=$1",
        ),
    ] {
        let mut cursor = String::new();
        loop {
            let rows = sqlx::query(select)
                .bind(&cursor)
                .fetch_all(&mut **tx)
                .await
                .map_err(database_error)?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                cursor = row.try_get("cursor").map_err(database_error)?;
                let user: String = row.try_get("user_id").map_err(database_error)?;
                let context: String = row.try_get("context").map_err(database_error)?;
                let nonce: Vec<u8> = row.try_get("nonce").map_err(database_error)?;
                let secret = EncryptedSecret {
                    nonce: nonce
                        .try_into()
                        .map_err(|_| HarnessError::execution("invalid MFA nonce"))?,
                    ciphertext: row.try_get("ciphertext").map_err(database_error)?,
                };
                let plaintext = current.decrypt(SCOPE, Some(&user), &context, 1, &secret)?;
                let encrypted = next.encrypt(SCOPE, Some(&user), &context, 1, &plaintext)?;
                sqlx::query(update)
                    .bind(&cursor)
                    .bind(encrypted.nonce.to_vec())
                    .bind(encrypted.ciphertext)
                    .execute(&mut **tx)
                    .await
                    .map_err(database_error)?;
                count += 1;
            }
        }
    }
    Ok(count)
}
