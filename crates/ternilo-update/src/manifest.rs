use std::{collections::BTreeSet, fs::File, io::Read, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ternilo_protocol::HarnessError;

pub const UPDATE_FORMAT: u32 = 1;
pub const RELEASE_ORIGIN: &str = "https://github.com/mosttt/ternilo/releases/download";
pub(crate) const MANIFEST_LIMIT: usize = 128 * 1024;
const ARCHIVE_LIMIT: u64 = 1024 * 1024 * 1024;
const TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub target: String,
    pub filename: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub format: u32,
    pub version: String,
    pub server_versions: String,
    pub minimum_client: String,
    /// Binary rollback is allowed only within the same persisted-data epoch.
    pub data_epoch: u32,
    pub artifacts: Vec<Artifact>,
}

/// Sign the exact UTF-8 manifest bytes, not a re-serialized interpretation of them.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRelease {
    pub manifest: String,
    pub signature: String,
}

#[derive(Clone, Debug)]
pub struct VerifiedRelease {
    signed: SignedRelease,
    manifest: ReleaseManifest,
}

impl SignedRelease {
    pub fn verify(&self) -> Result<VerifiedRelease, HarnessError> {
        let key = STANDARD
            .decode(include_str!("../release-public-key.txt").trim())
            .map_err(|_| HarnessError::execution("invalid built-in release verification key"))?;
        let key: [u8; 32] = key.try_into().map_err(|_| {
            HarnessError::execution("invalid built-in release verification key size")
        })?;
        self.verify_with_key(&key)
    }

    fn verify_with_key(&self, key: &[u8; 32]) -> Result<VerifiedRelease, HarnessError> {
        if self.manifest.len() > MANIFEST_LIMIT || self.signature.len() > 128 {
            return Err(HarnessError::invalid(
                "release manifest or signature exceeds its limit",
            ));
        }
        let signature = STANDARD
            .decode(self.signature.trim())
            .map_err(|_| HarnessError::policy("release signature is not valid base64"))?;
        let signature = Signature::from_slice(&signature)
            .map_err(|_| HarnessError::policy("release signature has an invalid size"))?;
        VerifyingKey::from_bytes(key)
            .map_err(|_| HarnessError::policy("release verification key is invalid"))?
            .verify_strict(self.manifest.as_bytes(), &signature)
            .map_err(|_| HarnessError::policy("release signature verification failed"))?;
        let manifest: ReleaseManifest = serde_json::from_str(&self.manifest)
            .map_err(|_| HarnessError::invalid("signed release manifest is invalid"))?;
        manifest.validate()?;
        Ok(VerifiedRelease {
            signed: self.clone(),
            manifest,
        })
    }
}

impl ReleaseManifest {
    fn validate(&self) -> Result<(), HarnessError> {
        if self.format != UPDATE_FORMAT || self.data_epoch == 0 {
            return Err(HarnessError::invalid(
                "unsupported release manifest format or data epoch",
            ));
        }
        stable_version(&self.version)?;
        stable_version(&self.minimum_client)?;
        VersionReq::parse(&self.server_versions)
            .map_err(|_| HarnessError::invalid("release Server compatibility is invalid"))?;
        if self.artifacts.is_empty() || self.artifacts.len() > TARGETS.len() {
            return Err(HarnessError::invalid(
                "release must contain supported client artifacts",
            ));
        }
        let mut targets = BTreeSet::new();
        for artifact in &self.artifacts {
            let extension = if artifact.target.ends_with("windows-msvc") {
                "zip"
            } else {
                "tar.gz"
            };
            let expected = format!("ternilo-{}-{}.{}", self.version, artifact.target, extension);
            if !TARGETS.contains(&artifact.target.as_str())
                || !targets.insert(&artifact.target)
                || artifact.filename != expected
                || artifact.bytes == 0
                || artifact.bytes > ARCHIVE_LIMIT
                || artifact.sha256.len() != 64
                || !artifact
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(HarnessError::invalid(
                    "invalid or duplicate signed client artifact",
                ));
            }
        }
        Ok(())
    }
}

impl VerifiedRelease {
    #[must_use]
    pub fn manifest(&self) -> &ReleaseManifest {
        &self.manifest
    }
    #[must_use]
    pub fn signed(&self) -> &SignedRelease {
        &self.signed
    }

    pub fn artifact(&self, target: &str) -> Result<&Artifact, HarnessError> {
        self.manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.target == target)
            .ok_or_else(|| HarnessError::invalid("release does not support this client platform"))
    }

    pub fn validate_upgrade(
        &self,
        client: &str,
        server: &str,
        data_epoch: u32,
    ) -> Result<(), HarnessError> {
        let current = Version::parse(client)
            .map_err(|_| HarnessError::invalid("invalid current client version"))?;
        let target = stable_version(&self.manifest.version)?;
        let minimum = stable_version(&self.manifest.minimum_client)?;
        if current < minimum {
            return Err(HarnessError::conflict(
                "update this client manually once before enabling remote upgrades",
            ));
        }
        if target <= current {
            return Err(HarnessError::conflict(
                "remote upgrades cannot reinstall or downgrade a client",
            ));
        }
        let server =
            Version::parse(server).map_err(|_| HarnessError::invalid("invalid Server version"))?;
        let compatible = VersionReq::parse(&self.manifest.server_versions)
            .map_err(|_| HarnessError::invalid("invalid Server compatibility"))?;
        if !compatible.matches(&server) || data_epoch != self.manifest.data_epoch {
            return Err(HarnessError::conflict(
                "release requires a separate maintenance upgrade; Server or data format is incompatible",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn artifact_url(&self, artifact: &Artifact) -> String {
        format!(
            "{RELEASE_ORIGIN}/v{}/{}",
            self.manifest.version, artifact.filename
        )
    }
}

pub(crate) fn stable_version(value: &str) -> Result<Version, HarnessError> {
    let version = Version::parse(value)
        .map_err(|_| HarnessError::invalid("release version must be semantic"))?;
    if !version.pre.is_empty() || !version.build.is_empty() || version.to_string() != value {
        return Err(HarnessError::invalid(
            "remote upgrades require a stable release version",
        ));
    }
    Ok(version)
}

pub fn verify_file(path: &Path, artifact: &Artifact) -> Result<(), HarnessError> {
    let mut file = File::open(path).map_err(io_error)?;
    if file.metadata().map_err(io_error)?.len() != artifact.bytes {
        return Err(HarnessError::policy(
            "update package size does not match its signed manifest",
        ));
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if hex_digest(&digest.finalize()) != artifact.sha256 {
        return Err(HarnessError::policy(
            "update package checksum does not match its signed manifest",
        ));
    }
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a string");
            output
        },
    )
}

pub(crate) fn io_error(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::execution(format!("client update storage: {error}"))
}

#[must_use]
pub fn current_target() -> Option<&'static str> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") if cfg!(target_env = "gnu") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") if cfg!(target_env = "gnu") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        _ => return None,
    };
    Some(target)
}

#[cfg(test)]
pub(crate) fn fixture_release(bytes: &[u8], target: &str) -> VerifiedRelease {
    use ed25519_dalek::{Signer, SigningKey};
    let key = SigningKey::from_bytes(&[17; 32]);
    let suffix = if target.ends_with("windows-msvc") {
        "zip"
    } else {
        "tar.gz"
    };
    let manifest = serde_json::json!({"format":1,"version":"0.2.21","minimum_client":"0.2.20","server_versions":">=0.2.20, <0.3.0","data_epoch":1,
        "artifacts":[{"target":target,"filename":format!("ternilo-0.2.21-{target}.{suffix}"),"bytes":bytes.len(),"sha256":hex_digest(&Sha256::digest(bytes))}]}).to_string();
    let signature = STANDARD.encode(key.sign(manifest.as_bytes()).to_bytes());
    SignedRelease {
        manifest,
        signature,
    }
    .verify_with_key(&key.verifying_key().to_bytes())
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::io::Write;

    fn signed() -> (SignedRelease, [u8; 32]) {
        let key = SigningKey::from_bytes(&[13; 32]);
        let manifest = serde_json::json!({"format":1,"version":"0.2.21","minimum_client":"0.2.20",
            "server_versions":">=0.2.20, <0.3.0","data_epoch":1,"artifacts":[{
                "target":"x86_64-unknown-linux-gnu","filename":"ternilo-0.2.21-x86_64-unknown-linux-gnu.tar.gz",
                "bytes":3,"sha256":hex_digest(&Sha256::digest(b"abc"))}]}).to_string();
        let signature = STANDARD.encode(key.sign(manifest.as_bytes()).to_bytes());
        (
            SignedRelease {
                manifest,
                signature,
            },
            key.verifying_key().to_bytes(),
        )
    }

    #[test]
    fn verifies_original_bytes_and_rejects_changed_manifest_signature_and_wrong_root() {
        let (signed, key) = signed();
        let verified = signed.verify_with_key(&key).unwrap();
        assert_eq!(verified.manifest().version, "0.2.21");
        let mut changed = signed.clone();
        changed.manifest = changed.manifest.replace("0.2.21", "0.2.22");
        assert!(changed.verify_with_key(&key).is_err());
        changed = signed.clone();
        changed.signature = STANDARD.encode([0_u8; 64]);
        assert!(changed.verify_with_key(&key).is_err());
        assert!(
            signed.verify().is_err(),
            "fixture key must never be trusted in production"
        );
    }

    #[test]
    fn checks_compatibility_and_prevents_downgrade_or_implicit_migration() {
        let (signed, key) = signed();
        let release = signed.verify_with_key(&key).unwrap();
        release.validate_upgrade("0.2.20", "0.2.21", 1).unwrap();
        for (client, server, epoch) in [
            ("0.2.19", "0.2.21", 1),
            ("0.2.21", "0.2.21", 1),
            ("0.2.22", "0.2.21", 1),
            ("0.2.20", "0.3.0", 1),
            ("0.2.20", "0.2.21", 2),
        ] {
            assert!(release.validate_upgrade(client, server, epoch).is_err());
        }
        assert!(release.artifact("x86_64-pc-windows-msvc").is_err());
    }

    #[test]
    fn rejects_path_substitution_and_duplicate_platforms_even_when_signed() {
        let (signed, key) = signed();
        let release = signed.verify_with_key(&key).unwrap();
        let mut manifest = release.manifest.clone();
        manifest.artifacts[0].filename = "../../ternilo".into();
        assert!(manifest.validate().is_err());
        manifest = release.manifest.clone();
        manifest.artifacts.push(manifest.artifacts[0].clone());
        assert!(manifest.validate().is_err());
        manifest = release.manifest.clone();
        manifest.version = "0.2.21-rc.1".into();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn refuses_truncated_and_same_size_tampered_packages() {
        let (signed, key) = signed();
        let release = signed.verify_with_key(&key).unwrap();
        let artifact = &release.manifest.artifacts[0];
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"abc").unwrap();
        verify_file(file.path(), artifact).unwrap();
        std::fs::write(file.path(), b"abd").unwrap();
        assert!(verify_file(file.path(), artifact).is_err());
        std::fs::write(file.path(), b"ab").unwrap();
        assert!(verify_file(file.path(), artifact).is_err());
    }
}
