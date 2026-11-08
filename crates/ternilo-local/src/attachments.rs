use std::{
    fmt::Write as _,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use ternilo_kernel::AttachmentResolver;
use ternilo_protocol::{ATTACHMENT_REFERENCE_PREFIX, Attachment, HarnessError};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

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
    bytes: Option<Vec<u8>>,
}

enum WritePolicy {
    Upload,
    Retained(u64),
}

struct SessionAttachments {
    objects: Arc<LocalAttachments>,
    max_retained_bytes: u64,
}

impl LocalAttachments {
    pub async fn file_download_chunk(
        &self,
        attachment: &Attachment,
        offset: u64,
    ) -> Result<ternilo_protocol::FileDownloadChunk, HarnessError> {
        use ternilo_protocol::{
            FileDownloadChunk, FileDownloadVersion, WORKSPACE_DOWNLOAD_CHUNK_BYTES,
        };
        attachment.validate()?;
        let (bytes, total, digest) = if let Some(digest) = attachment.reference_digest() {
            let mut file = tokio::fs::File::open(self.reader.object_path(digest))
                .await
                .map_err(|error| HarnessError::execution(format!("open retained file: {error}")))?;
            let total = file
                .metadata()
                .await
                .map_err(|error| {
                    HarnessError::execution(format!("inspect retained file: {error}"))
                })?
                .len();
            if offset > total {
                return Err(HarnessError::invalid(
                    "download offset exceeds the file size",
                ));
            }
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|error| HarnessError::execution(format!("seek retained file: {error}")))?;
            let mut bytes = Vec::new();
            file.take(WORKSPACE_DOWNLOAD_CHUNK_BYTES)
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| HarnessError::execution(format!("read retained file: {error}")))?;
            (bytes, total, digest.to_owned())
        } else {
            let bytes = inline_file_attachment_bytes(attachment)?;
            let digest = hex_digest(&bytes);
            let start = usize::try_from(offset)
                .ok()
                .filter(|offset| *offset <= bytes.len())
                .ok_or_else(|| HarnessError::invalid("download offset exceeds the file size"))?;
            let end = start
                .saturating_add(
                    usize::try_from(WORKSPACE_DOWNLOAD_CHUNK_BYTES)
                        .expect("1 MiB chunk fits usize"),
                )
                .min(bytes.len());
            (bytes[start..end].to_vec(), bytes.len() as u64, digest)
        };
        Ok(FileDownloadChunk {
            offset,
            version: FileDownloadVersion {
                bytes: total,
                revision: digest.clone(),
            },
            sha256: Some(digest),
            content_base64: STANDARD.encode(bytes),
        })
    }

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
        self.save_many_with_limit(attachments, WritePolicy::Upload)
            .await
    }

    async fn save_many_with_limit(
        &self,
        attachments: Vec<Attachment>,
        policy: WritePolicy,
    ) -> Result<Vec<Attachment>, HarnessError> {
        let max_bytes = match policy {
            WritePolicy::Upload => MAX_ATTACHMENT_BYTES,
            WritePolicy::Retained(bytes) => bytes,
        };
        let upload = matches!(policy, WritePolicy::Upload);
        let mut prepared = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            if upload || attachment.is_reference() {
                attachment.validate()?;
            } else if attachment.name.trim().is_empty() || attachment.media_type.trim().is_empty() {
                return Err(HarnessError::invalid(
                    "retained attachment name and media type must not be empty",
                ));
            }
            if attachment.is_reference() {
                let bytes = self.reference_bytes(&attachment).await?;
                if upload && bytes.is_empty() {
                    return Err(HarnessError::invalid(
                        "uploaded attachment must not be empty",
                    ));
                }
                prepared.push(PreparedAttachment {
                    digest: attachment
                        .reference_digest()
                        .expect("reference was checked")
                        .to_owned(),
                    reference: attachment,
                    bytes: None,
                });
                continue;
            }
            let bytes = decode_file_content(&attachment)?;
            if upload && bytes.is_empty() {
                return Err(HarnessError::invalid(
                    "uploaded attachment must not be empty",
                ));
            }
            if bytes.len() as u64 > max_bytes {
                return Err(HarnessError::invalid(format!(
                    "decoded attachment exceeds {max_bytes} bytes"
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
                bytes: Some(bytes),
            });
        }

        for item in &prepared {
            if let Some(bytes) = &item.bytes {
                self.publish(&item.digest, bytes).await?;
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
            self.save_many_with_limit(
                vec![attachment],
                WritePolicy::Retained(MAX_RETAINED_OUTPUT_BYTES),
            )
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

impl LocalAttachments {
    pub fn with_retained_file_limit(
        self: &Arc<Self>,
        max_retained_bytes: u64,
    ) -> Arc<dyn AttachmentResolver> {
        Arc::new(SessionAttachments {
            objects: Arc::clone(self),
            max_retained_bytes,
        })
    }
}

impl AttachmentResolver for SessionAttachments {
    fn store<'a>(
        &'a self,
        attachment: Attachment,
    ) -> Pin<Box<dyn Future<Output = Result<Attachment, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.objects
                .save_many_with_limit(
                    vec![attachment],
                    WritePolicy::Retained(self.max_retained_bytes),
                )
                .await?
                .pop()
                .ok_or_else(|| HarnessError::execution("attachment store returned no object"))
        })
    }

    fn resolve<'a>(
        &'a self,
        attachment: Attachment,
    ) -> Pin<Box<dyn Future<Output = Result<Attachment, HarnessError>> + Send + 'a>> {
        Box::pin(self.objects.resolve_attachment(attachment))
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
    async fn streamed_retained_files_verify_the_canonical_digest() {
        use futures_util::TryStreamExt as _;
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(LocalAttachments::open(root.path()).await.unwrap());
        let reference = store
            .store(Attachment {
                name: "proof.txt".into(),
                media_type: "text/plain".into(),
                content: "proof".into(),
            })
            .await
            .unwrap();
        tokio::fs::write(
            store
                .reader
                .object_path(reference.reference_digest().unwrap()),
            b"other",
        )
        .await
        .unwrap();
        let first = store.file_download_chunk(&reference, 0).await.unwrap();
        let stream = crate::workspace_download_stream(first, |_, _| async {
            panic!("small file is one chunk")
        });
        assert!(
            stream
                .try_collect::<Vec<_>>()
                .await
                .unwrap_err()
                .message
                .contains("digest verification")
        );
    }

    #[tokio::test]
    async fn retained_file_policy_is_scoped_and_lowering_it_preserves_accepted_objects() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(LocalAttachments::open(root.path()).await.unwrap());
        let raised = store.with_retained_file_limit(66 * 1024 * 1024);
        let reference = raised
            .store(Attachment {
                name: "large.txt".into(),
                media_type: "text/plain".into(),
                content: "r".repeat(65 * 1024 * 1024),
            })
            .await
            .unwrap();
        let lower = store.with_retained_file_limit(1024);
        assert!(
            lower
                .store(Attachment {
                    name: "new.txt".into(),
                    media_type: "text/plain".into(),
                    content: "n".repeat(1025)
                })
                .await
                .is_err()
        );
        assert_eq!(
            lower
                .resolve(reference.clone())
                .await
                .unwrap()
                .content
                .len(),
            65 * 1024 * 1024
        );
        let restored_root = tempfile::tempdir().unwrap();
        let restored = LocalAttachments::open(restored_root.path()).await.unwrap();
        restored
            .restore_reference(
                &reference,
                &store.reference_bytes(&reference).await.unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            restored.reference_bytes(&reference).await.unwrap().len(),
            65 * 1024 * 1024
        );
        // Choosing the same numeric limit as uploads must not apply upload normalization.
        let generated = store.with_retained_file_limit(MAX_ATTACHMENT_BYTES);
        assert!(
            generated
                .store(Attachment {
                    name: "empty.txt".into(),
                    media_type: "text/plain".into(),
                    content: String::new()
                })
                .await
                .unwrap()
                .is_reference()
        );
    }

    #[tokio::test]
    async fn zero_byte_file_objects_restore_without_allowing_empty_uploads() {
        let root = tempfile::tempdir().unwrap();
        let store = LocalAttachments::open(root.path()).await.unwrap();
        let file = Attachment {
            name: "empty.txt".into(),
            media_type: "text/plain".into(),
            content: String::new(),
        };
        assert!(store.save_many(vec![file.clone()]).await.is_err());
        let reference = store.store(file.clone()).await.unwrap();
        assert!(reference.is_reference());
        assert!(store.reference_bytes(&reference).await.unwrap().is_empty());
        assert_eq!(
            store.resolve_attachment(reference.clone()).await.unwrap(),
            file
        );
        assert!(store.save_many(vec![reference.clone()]).await.is_err());
        let destination = tempfile::tempdir().unwrap();
        let restored = LocalAttachments::open(destination.path()).await.unwrap();
        restored.restore_reference(&reference, &[]).await.unwrap();
        assert!(
            restored
                .reference_bytes(&reference)
                .await
                .unwrap()
                .is_empty()
        );
    }

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
