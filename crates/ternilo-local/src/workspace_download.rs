use std::{fmt::Write as _, future::Future};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{Stream, stream};
use sha2::{Digest, Sha256};
use ternilo_protocol::{
    FileDownloadChunk, FileDownloadVersion, HarnessError, WORKSPACE_DOWNLOAD_CHUNK_BYTES,
};

/// Pull one bounded chunk at a time; dropping the HTTP body stops further reads.
pub fn workspace_download_stream<F, Fut>(
    first: FileDownloadChunk,
    read: F,
) -> impl Stream<Item = Result<Vec<u8>, HarnessError>>
where
    F: FnMut(u64, FileDownloadVersion) -> Fut,
    Fut: Future<Output = Result<FileDownloadChunk, HarnessError>>,
{
    let version = first.version.clone();
    let expected_digest = first.sha256.clone();
    let hasher = expected_digest.as_ref().map(|_| Sha256::new());
    stream::try_unfold(
        (Some(first), read, 0_u64, version, expected_digest, hasher),
        |(mut first, mut read, offset, version, expected_digest, mut hasher)| async move {
            if first.is_none() && offset == version.bytes {
                return Ok(None);
            }
            let chunk = match first.take() {
                Some(chunk) => chunk,
                None => read(offset, version.clone()).await?,
            };
            if chunk.offset != offset
                || chunk.version != version
                || chunk.sha256 != expected_digest
                || chunk.content_base64.len() as u64
                    > WORKSPACE_DOWNLOAD_CHUNK_BYTES.div_ceil(3) * 4
            {
                return Err(HarnessError::conflict(
                    "workspace download chunk does not match the requested file",
                ));
            }
            let bytes = STANDARD.decode(&chunk.content_base64).map_err(|error| {
                HarnessError::execution(format!("decode workspace download: {error}"))
            })?;
            let next = offset
                .checked_add(bytes.len() as u64)
                .filter(|next| *next <= version.bytes)
                .ok_or_else(|| {
                    HarnessError::conflict("workspace download exceeds its declared size")
                })?;
            if bytes.is_empty() && next < version.bytes {
                return Err(HarnessError::conflict(
                    "workspace file ended before the download completed",
                ));
            }
            if let Some(digest) = &mut hasher {
                digest.update(&bytes);
                if next == version.bytes {
                    let mut actual = String::with_capacity(64);
                    for byte in digest.clone().finalize() {
                        write!(actual, "{byte:02x}").expect("write digest to String");
                    }
                    if Some(actual) != expected_digest {
                        return Err(HarnessError::execution(
                            "retained file failed digest verification",
                        ));
                    }
                }
            }
            Ok(Some((
                bytes,
                (first, read, next, version, expected_digest, hasher),
            )))
        },
    )
}
