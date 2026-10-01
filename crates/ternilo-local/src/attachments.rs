use std::{
    fmt::Write as _,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use ternilo_kernel::AttachmentResolver;
use ternilo_protocol::{ATTACHMENT_REFERENCE_PREFIX, Attachment, HarnessError};
use tokio::io::AsyncWriteExt;

const MAX_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RETAINED_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;

pub struct LocalAttachments {
    reader: LocalAttachmentReader,
    next_temporary: AtomicU64,
}

/// Read-only view of attachment objects initialized and written by another
/// process. Constructing it never creates or changes workspace files.
pub struct LocalAttachmentReader {
    objects: PathBuf,
}

struct PreparedAttachment {
    reference: Attachment,
    digest: String,
    bytes: Vec<u8>,
}

impl LocalAttachments {
    pub async fn open(data_root: &Path) -> Result<Self, HarnessError> {
        let reader = LocalAttachmentReader::new(data_root);
        tokio::fs::create_dir_all(&reader.objects)
            .await
            .map_err(|error| {
                HarnessError::execution(format!(
                    "create attachment object directory {}: {error}",
                    reader.objects.display()
                ))
            })?;
        set_private_directory(&reader.objects).await?;
        Ok(Self {
            reader,
            next_temporary: AtomicU64::new(1),
        })
    }

    pub async fn save_many(
        &self,
        attachments: Vec<Attachment>,
    ) -> Result<Vec<Attachment>, HarnessError> {
        self.save_many_with_limit(attachments, MAX_ATTACHMENT_BYTES)
            .await
    }

    async fn save_many_with_limit(
        &self,
        attachments: Vec<Attachment>,
        max_bytes: u64,
    ) -> Result<Vec<Attachment>, HarnessError> {
        let mut prepared = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            if max_bytes == MAX_ATTACHMENT_BYTES || attachment.is_reference() {
                attachment.validate()?;
            } else if attachment.name.trim().is_empty()
                || attachment.media_type.trim().is_empty()
                || attachment.content.is_empty()
            {
                return Err(HarnessError::invalid(
                    "retained attachment name, media type, and content must not be empty",
                ));
            }
            if attachment.is_reference() {
                self.resolve_attachment(attachment.clone()).await?;
                prepared.push(PreparedAttachment {
                    digest: attachment
                        .reference_digest()
                        .expect("reference was checked")
                        .to_owned(),
                    reference: attachment,
                    bytes: Vec::new(),
                });
                continue;
            }
            let bytes = decode_file_content(&attachment)?;
            if bytes.is_empty() || bytes.len() as u64 > max_bytes {
                return Err(HarnessError::invalid(format!(
                    "decoded attachment must contain 1 byte to {max_bytes} bytes"
                )));
            }
            let digest = hex_digest(&bytes);
            prepared.push(PreparedAttachment {
                reference: Attachment {
                    name: attachment.name,
                    media_type: attachment.media_type,
                    content: format!("{ATTACHMENT_REFERENCE_PREFIX}{digest}"),
                },
                digest,
                bytes,
            });
        }

        for item in &prepared {
            if !item.bytes.is_empty() {
                self.publish(&item.digest, &item.bytes).await?;
            }
        }
        Ok(prepared.into_iter().map(|item| item.reference).collect())
    }

    pub async fn resolve_attachment(
        &self,
        attachment: Attachment,
    ) -> Result<Attachment, HarnessError> {
        attachment.validate()?;
        if attachment.reference_digest().is_none() {
            return Ok(attachment);
        }
        let bytes = self.reference_bytes(&attachment).await?;
        let content = if attachment.media_type.starts_with("image/") {
            format!(
                "data:{};base64,{}",
                attachment.media_type,
                STANDARD.encode(&bytes)
            )
        } else if let Ok(text) = String::from_utf8(bytes.clone()) {
            text
        } else {
            format!(
                "data:{};base64,{}",
                attachment.media_type,
                STANDARD.encode(bytes)
            )
        };
        Ok(Attachment {
            name: attachment.name,
            media_type: attachment.media_type,
            content,
        })
    }

    /// Reads and verifies a retained attachment reference without changing its
    /// representation. Cloud workers use this to copy workspace objects into
    /// the tenant-scoped durable object store before publishing an event.
    pub async fn reference_bytes(&self, attachment: &Attachment) -> Result<Vec<u8>, HarnessError> {
        self.reader.reference_bytes(attachment).await
    }

    /// Restore an already retained object without applying upload normalization.
    pub async fn restore_reference(
        &self,
        attachment: &Attachment,
        bytes: &[u8],
    ) -> Result<(), HarnessError> {
        attachment.validate()?;
        let digest = attachment.reference_digest().ok_or_else(|| {
            HarnessError::invalid("attachment restore requires a retained object reference")
        })?;
        let size = u64::try_from(bytes.len())
            .map_err(|_| HarnessError::invalid("attachment object exceeds retained size limit"))?;
        if size == 0 || size > MAX_RETAINED_OUTPUT_BYTES {
            return Err(HarnessError::invalid(
                "attachment object has an invalid retained size",
            ));
        }
        if hex_digest(bytes) != digest {
            return Err(HarnessError::policy(
                "attachment object does not match its canonical digest",
            ));
        }
        self.publish(digest, bytes).await
    }

    async fn publish(&self, digest: &str, bytes: &[u8]) -> Result<(), HarnessError> {
        let path = self.reader.object_path(digest);
        if tokio::fs::try_exists(&path).await.map_err(|error| {
            HarnessError::execution(format!(
                "inspect attachment object {}: {error}",
                path.display()
            ))
        })? {
            let existing = tokio::fs::read(&path).await.map_err(|error| {
                HarnessError::execution(format!(
                    "read existing attachment object {}: {error}",
                    path.display()
                ))
            })?;
            if hex_digest(&existing) == digest {
                return Ok(());
            }
            return Err(HarnessError::execution(format!(
                "attachment object {digest} is corrupt"
            )));
        }

        let sequence = self.next_temporary.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_extension(format!("tmp-{}-{sequence}", std::process::id()));
        let mut options = tokio::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).await.map_err(|error| {
            HarnessError::execution(format!(
                "create attachment object temporary {}: {error}",
                temporary.display()
            ))
        })?;
        file.write_all(bytes).await.map_err(|error| {
            HarnessError::execution(format!(
                "write attachment object temporary {}: {error}",
                temporary.display()
            ))
        })?;
        file.sync_all().await.map_err(|error| {
            HarnessError::execution(format!(
                "sync attachment object temporary {}: {error}",
                temporary.display()
            ))
        })?;
        drop(file);
        tokio::fs::rename(&temporary, &path)
            .await
            .map_err(|error| {
                HarnessError::execution(format!(
                    "publish attachment object {}: {error}",
                    path.display()
                ))
            })?;
        set_private_file(&path).await?;
        crate::persistence::sync_parent_directory(&path).await
    }
}

impl LocalAttachmentReader {
    #[must_use]
    pub fn new(data_root: &Path) -> Self {
        Self {
            objects: data_root.join("data/attachments").join("objects"),
        }
    }

    pub async fn reference_bytes(&self, attachment: &Attachment) -> Result<Vec<u8>, HarnessError> {
        attachment.validate()?;
        let digest = attachment.reference_digest().ok_or_else(|| {
            HarnessError::invalid("attachment must be a retained object reference")
        })?;
        let path = self.object_path(digest);
        let metadata = tokio::fs::metadata(&path).await.map_err(|error| {
            HarnessError::execution(format!(
                "read attachment object metadata {}: {error}",
                path.display()
            ))
        })?;
        if metadata.len() == 0 || metadata.len() > MAX_RETAINED_OUTPUT_BYTES {
            return Err(HarnessError::execution(format!(
                "attachment object {digest} has an invalid size"
            )));
        }
        let bytes = tokio::fs::read(&path).await.map_err(|error| {
            HarnessError::execution(format!(
                "read attachment object {}: {error}",
                path.display()
            ))
        })?;
        if hex_digest(&bytes) != digest {
            return Err(HarnessError::execution(format!(
                "attachment object {digest} failed digest verification"
            )));
        }
        Ok(bytes)
    }

    fn object_path(&self, digest: &str) -> PathBuf {
        self.objects.join(digest)
    }
}

impl AttachmentResolver for LocalAttachments {
    fn store<'a>(
        &'a self,
        attachment: Attachment,
    ) -> Pin<Box<dyn Future<Output = Result<Attachment, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.save_many_with_limit(vec![attachment], MAX_RETAINED_OUTPUT_BYTES)
                .await?
                .pop()
                .ok_or_else(|| HarnessError::execution("attachment store returned no object"))
        })
    }

    fn resolve<'a>(
        &'a self,
        attachment: Attachment,
    ) -> Pin<Box<dyn Future<Output = Result<Attachment, HarnessError>> + Send + 'a>> {
        Box::pin(self.resolve_attachment(attachment))
    }
}

/// Decode legacy inline file facts without mistaking ordinary text for a data URL.
pub fn inline_file_attachment_bytes(attachment: &Attachment) -> Result<Vec<u8>, HarnessError> {
    attachment.validate()?;
    decode_file_content(attachment)
}

fn decode_file_content(attachment: &Attachment) -> Result<Vec<u8>, HarnessError> {
    if attachment.is_text() {
        return Ok(attachment.content.as_bytes().to_vec());
    }
    decode_content(attachment)
}

fn decode_content(attachment: &Attachment) -> Result<Vec<u8>, HarnessError> {
    if !attachment.content.starts_with("data:") {
        return Ok(attachment.content.as_bytes().to_vec());
    }
    let (header, encoded) = attachment
        .content
        .split_once(',')
        .ok_or_else(|| HarnessError::invalid("attachment data URL has no payload"))?;
    let expected = format!("data:{};base64", attachment.media_type);
    if header != expected {
        return Err(HarnessError::invalid(
            "attachment data URL media type must match attachment media_type and use base64",
        ));
    }
    STANDARD
        .decode(encoded)
        .map_err(|error| HarnessError::invalid(format!("decode attachment base64: {error}")))
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut digest, byte| {
            write!(digest, "{byte:02x}").expect("writing to a String cannot fail");
            digest
        })
}

#[cfg(unix)]
async fn set_private_directory(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|error| {
            HarnessError::execution(format!(
                "set private attachment directory permissions {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
async fn set_private_directory(_: &Path) -> Result<(), HarnessError> {
    Ok(())
}

#[cfg(unix)]
async fn set_private_file(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|error| {
            HarnessError::execution(format!(
                "set private attachment permissions {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
async fn set_private_file(_: &Path) -> Result<(), HarnessError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn stores_deduplicated_private_objects_and_resolves_references() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(LocalAttachments::open(root.path()).await.unwrap());
        let original = Attachment {
            name: "note.txt".to_owned(),
            media_type: "text/plain".to_owned(),
            content: "durable payload".to_owned(),
        };
        let first = store.save_many(vec![original.clone()]).await.unwrap();
        let second = store.save_many(vec![original.clone()]).await.unwrap();
        assert_eq!(first, second);
        assert!(first[0].is_reference());
        assert_eq!(
            store.resolve_attachment(first[0].clone()).await.unwrap(),
            original
        );
        assert_eq!(
            store.reference_bytes(&first[0]).await.unwrap(),
            b"durable payload"
        );
        let mut directory = tokio::fs::read_dir(&store.reader.objects).await.unwrap();
        let mut count = 0;
        while directory.next_entry().await.unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn reader_waits_for_the_workspace_owner_to_initialize_objects() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join(".ternilo");
        let reader = LocalAttachmentReader::new(&data_root);
        assert!(!data_root.exists());

        let store = LocalAttachments::open(&data_root).await.unwrap();
        let stored = store
            .save_many(vec![Attachment {
                name: "later.txt".to_owned(),
                media_type: "text/plain".to_owned(),
                content: "written by the workspace owner".to_owned(),
            }])
            .await
            .unwrap();

        assert_eq!(
            reader.reference_bytes(&stored[0]).await.unwrap(),
            b"written by the workspace owner"
        );
    }
}
