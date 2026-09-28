use ternilo_protocol::{HarnessError, TenantId, UserId};
use zeroize::Zeroizing;

use crate::{ControlStore, EncryptedSecret};

impl ControlStore {
    /// Decrypt an owner's credential after the trusted host has authorized the
    /// canonical run. This primitive is never exposed as a remote operation.
    pub fn decrypt_user_secret(
        &self,
        tenant_id: &TenantId,
        user_id: &UserId,
        name: &str,
        version: u64,
        encrypted: &EncryptedSecret,
    ) -> Result<Zeroizing<Vec<u8>>, HarnessError> {
        tenant_id.validate()?;
        user_id.validate()?;
        self.cipher.decrypt(
            tenant_id.as_str(),
            Some(user_id.as_str()),
            name,
            version,
            encrypted,
        )
    }
}
