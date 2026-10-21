use crate::{
    Artifact, RELEASE_ORIGIN, SignedRelease, VerifiedRelease,
    manifest::{MANIFEST_LIMIT, io_error, stable_version},
    verify_file,
};
use futures_util::StreamExt as _;
use std::{path::Path, time::Duration};
use ternilo_protocol::HarnessError;
use tokio::io::AsyncWriteExt as _;

fn client() -> Result<reqwest::Client, HarnessError> {
    reqwest::Client::builder()
        .user_agent(concat!("Ternilo/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_mins(10))
        .build()
        .map_err(|error| HarnessError::execution(format!("create update download client: {error}")))
}

async fn small_document(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
) -> Result<String, HarnessError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(network_error)?
        .error_for_status()
        .map_err(network_error)?;
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(network_error)?;
        if body.len() + chunk.len() > limit {
            return Err(HarnessError::invalid(
                "release metadata exceeds its size limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| HarnessError::invalid("release metadata is not UTF-8"))
}

pub async fn fetch_release(version: &str) -> Result<VerifiedRelease, HarnessError> {
    stable_version(version)?;
    let client = client()?;
    let base = format!("{RELEASE_ORIGIN}/v{version}/client-updates.json");
    let signature_url = format!("{base}.sig");
    let (manifest, signature) = tokio::try_join!(
        small_document(&client, &base, MANIFEST_LIMIT),
        small_document(&client, &signature_url, 128)
    )?;
    let release = SignedRelease {
        manifest,
        signature,
    }
    .verify()?;
    if release.manifest().version != version {
        return Err(HarnessError::policy(
            "release manifest belongs to a different requested version",
        ));
    }
    Ok(release)
}

/// Publish only complete, signed-content-verified downloads to the caller's cache path.
pub async fn download_artifact(
    release: &VerifiedRelease,
    target: &str,
    destination: &Path,
) -> Result<(), HarnessError> {
    let artifact = release.artifact(target)?.clone();
    let client = client()?;
    let response = client
        .get(release.artifact_url(&artifact))
        .send()
        .await
        .map_err(network_error)?
        .error_for_status()
        .map_err(network_error)?;
    save_response(response, &artifact, destination).await
}

pub async fn save_response(
    response: reqwest::Response,
    artifact: &Artifact,
    destination: &Path,
) -> Result<(), HarnessError> {
    let directory = destination
        .parent()
        .ok_or_else(|| HarnessError::invalid("update cache path has no parent"))?;
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(io_error)?;
    let temporary = tempfile::NamedTempFile::new_in(directory).map_err(io_error)?;
    let mut output = tokio::fs::File::from_std(temporary.reopen().map_err(io_error)?);
    let mut stream = response.bytes_stream();
    let mut bytes = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(network_error)?;
        bytes = bytes.saturating_add(chunk.len() as u64);
        if bytes > artifact.bytes {
            return Err(HarnessError::policy(
                "update download exceeds its signed size",
            ));
        }
        output.write_all(&chunk).await.map_err(io_error)?;
    }
    output.sync_all().await.map_err(io_error)?;
    drop(output);
    let path = temporary.path().to_owned();
    let expected = artifact.clone();
    tokio::task::spawn_blocking(move || verify_file(&path, &expected))
        .await
        .map_err(|error| HarnessError::execution(format!("verify update download: {error}")))??;
    temporary
        .persist(destination)
        .map_err(|error| io_error(error.error))?;
    Ok(())
}

fn network_error(error: reqwest::Error) -> HarnessError {
    // Download credentials and redirect URLs must never appear in user-visible diagnostics.
    HarnessError::unavailable(format!(
        "client update download failed: {}",
        error.without_url()
    ))
}
