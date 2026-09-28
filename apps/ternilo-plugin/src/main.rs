#![forbid(unsafe_code)]

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::ExitCode,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::{Parser, Subcommand};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use ternilo_extension::{
    ExtensionHostPolicy, ExtensionInstallRequest, ExtensionManifest, ExtensionPayload,
    ExtensionRegistry, ExtensionRuntime, PublisherTrust, SignedExtensionBundle, sign_bundle,
    verify_bundle,
};
use ternilo_protocol::HarnessError;
use zeroize::Zeroize as _;

const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;

#[derive(Parser)]
#[command(about = "Build, sign, and verify Ternilo Extension Packages", version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate an Ed25519 publisher key. Refuses to overwrite a file.
    Keygen {
        #[arg(long)]
        key_id: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Export a public trust record from a private publisher key.
    Publisher {
        #[arg(long)]
        key: PathBuf,
        #[arg(long = "source", required = true)]
        sources: Vec<String>,
        #[arg(long)]
        output: PathBuf,
    },
    /// Build a WASM Component payload for the wasm-component runtime.
    Componentize {
        #[arg(long)]
        module: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Sign one manifest and its runtime-specific payload as an Extension bundle.
    Sign {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        payload: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Verify signature, digest, host limits, and runtime compilation.
    Verify {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        publisher: PathBuf,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublisherKeyFile {
    schema_version: u32,
    key_id: String,
    private_key_base64: String,
    public_key_base64: String,
}

fn main() -> ExitCode {
    match execute(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn execute(args: Args) -> Result<(), HarnessError> {
    match args.command {
        Command::Keygen { key_id, output } => keygen(key_id, &output),
        Command::Publisher {
            key,
            sources,
            output,
        } => export_publisher(&key, sources, &output),
        Command::Componentize { module, output } => componentize(&module, &output),
        Command::Sign {
            manifest,
            payload,
            key,
            output,
        } => sign(&manifest, &payload, &key, &output),
        Command::Verify { bundle, publisher } => verify(&bundle, &publisher),
    }
}

fn keygen(key_id: String, output: &Path) -> Result<(), HarnessError> {
    let seed: [u8; 32] = rand::random();
    let signing_key = SigningKey::from_bytes(&seed);
    let file = PublisherKeyFile {
        schema_version: 1,
        key_id,
        private_key_base64: STANDARD.encode(seed),
        public_key_base64: STANDARD.encode(signing_key.verifying_key().to_bytes()),
    };
    write_json_new(output, &file, true)
}

fn export_publisher(
    key_path: &Path,
    sources: Vec<String>,
    output: &Path,
) -> Result<(), HarnessError> {
    let key = read_key(key_path)?;
    let publisher = PublisherTrust {
        key_id: key.key_id,
        public_key_base64: key.public_key_base64,
        allowed_sources: sources.into_iter().collect(),
    };
    publisher.validate()?;
    write_json_new(output, &publisher, false)
}

fn componentize(module: &Path, output: &Path) -> Result<(), HarnessError> {
    let maximum = extension_payload_limit();
    let module = read_bounded(module, maximum, "WASM core module")?;
    let component = wit_component::ComponentEncoder::default()
        .module(&module)
        .map_err(|error| HarnessError::invalid(format!("decode component metadata: {error:#}")))?
        .validate(true)
        .encode()
        .map_err(|error| HarnessError::invalid(format!("encode WASM component: {error:#}")))?;
    if component.len() > maximum {
        return Err(HarnessError::policy(
            "componentized WASM artifact exceeds the extension payload size limit",
        ));
    }
    write_new(output, &component, false)
}

fn sign(
    manifest_path: &Path,
    payload_path: &Path,
    key_path: &Path,
    output: &Path,
) -> Result<(), HarnessError> {
    let manifest: ExtensionManifest = read_json(manifest_path, MAX_METADATA_BYTES)?;
    let payload = read_extension_payload(&manifest.runtime, payload_path)?;
    let mut key = read_key(key_path)?;
    if manifest.publisher_key_id != key.key_id {
        key.private_key_base64.zeroize();
        return Err(HarnessError::policy(
            "manifest publisher_key_id does not match the signing key",
        ));
    }
    let decoded = STANDARD.decode(&key.private_key_base64);
    key.private_key_base64.zeroize();
    let mut private: [u8; 32] = decoded
        .map_err(|_| HarnessError::invalid("publisher private key is not valid base64"))?
        .try_into()
        .map_err(|_| HarnessError::invalid("publisher private key must contain 32 bytes"))?;
    let signing_key = SigningKey::from_bytes(&private);
    private.zeroize();
    let bundle = sign_bundle(manifest, payload, &signing_key)?;
    write_json_new(output, &bundle, false)
}

fn verify(bundle_path: &Path, publisher_path: &Path) -> Result<(), HarnessError> {
    let policy = ExtensionHostPolicy::default();
    let bundle: SignedExtensionBundle = read_json(bundle_path, maximum_bundle_bytes())?;
    let publisher: PublisherTrust = read_json(publisher_path, MAX_METADATA_BYTES)?;

    verify_bundle(&bundle, &publisher, policy.max_payload_bytes)?;
    compile_validate(&bundle, &publisher, policy)?;

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "verified": true,
            "package_id": bundle.manifest.package_id,
            "version": bundle.manifest.version,
            "source": bundle.manifest.source,
            "publisher_key_id": bundle.manifest.publisher_key_id,
            "payload_sha256": bundle.manifest.payload_sha256,
            "runtime": runtime_kind(&bundle.manifest.runtime),
            "requested_capabilities": bundle.manifest.requested_capabilities,
        }))
        .map_err(|error| HarnessError::execution(format!("encode verification result: {error}")))?
    );
    Ok(())
}

fn compile_validate(
    bundle: &SignedExtensionBundle,
    publisher: &PublisherTrust,
    policy: ExtensionHostPolicy,
) -> Result<(), HarnessError> {
    let directory = tempfile::tempdir().map_err(|error| {
        HarnessError::execution(format!("create temporary extension registry: {error}"))
    })?;
    let registry = ExtensionRegistry::open(directory.path().join("registry"), policy)?;
    registry.trust_publisher(publisher.clone(), 1)?;
    registry.install(
        ExtensionInstallRequest {
            bundle: bundle.clone(),
            granted_capabilities: std::collections::BTreeSet::default(),
        },
        2,
    )?;
    Ok(())
}

fn read_extension_payload(
    runtime: &ExtensionRuntime,
    path: &Path,
) -> Result<ExtensionPayload, HarnessError> {
    let bytes = read_bounded(path, extension_payload_limit(), "extension payload")?;
    match runtime {
        ExtensionRuntime::Rhai { .. } => String::from_utf8(bytes)
            .map(ExtensionPayload::Utf8)
            .map_err(|_| HarnessError::invalid("Rhai extension payload must be valid UTF-8")),
        ExtensionRuntime::WasmComponent { .. } => {
            Ok(ExtensionPayload::Base64(STANDARD.encode(bytes)))
        }
    }
}

const fn runtime_kind(runtime: &ExtensionRuntime) -> &'static str {
    match runtime {
        ExtensionRuntime::Rhai { .. } => "rhai",
        ExtensionRuntime::WasmComponent { .. } => "wasm-component",
    }
}

fn extension_payload_limit() -> usize {
    ExtensionHostPolicy::default().max_payload_bytes
}

fn maximum_bundle_bytes() -> usize {
    extension_payload_limit()
        .saturating_mul(2)
        .saturating_add(MAX_METADATA_BYTES)
}

fn read_key(path: &Path) -> Result<PublisherKeyFile, HarnessError> {
    let key: PublisherKeyFile = read_json(path, MAX_METADATA_BYTES)?;
    if key.schema_version != 1
        || key.key_id.trim().is_empty()
        || !STANDARD
            .decode(&key.public_key_base64)
            .is_ok_and(|bytes| bytes.len() == 32)
    {
        return Err(HarnessError::invalid("publisher key file is invalid"));
    }
    Ok(key)
}

fn read_json<T: DeserializeOwned>(path: &Path, maximum: usize) -> Result<T, HarnessError> {
    let bytes = read_bounded(path, maximum, "JSON input")?;
    serde_json::from_slice(&bytes)
        .map_err(|error| HarnessError::invalid(format!("parse {}: {error}", path.display())))
}

fn read_bounded(path: &Path, maximum: usize, label: &str) -> Result<Vec<u8>, HarnessError> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| HarnessError::invalid(format!("inspect {}: {error}", path.display())))?;
    if metadata.len() > maximum as u64 {
        return Err(HarnessError::invalid(format!(
            "{label} exceeds {maximum} bytes"
        )));
    }
    std::fs::read(path)
        .map_err(|error| HarnessError::invalid(format!("read {}: {error}", path.display())))
}

fn write_json_new<T: Serialize>(path: &Path, value: &T, private: bool) -> Result<(), HarnessError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| HarnessError::execution(format!("encode JSON output: {error}")))?;
    bytes.push(b'\n');
    write_new(path, &bytes, private)
}

fn write_new(path: &Path, bytes: &[u8], private: bool) -> Result<(), HarnessError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        HarnessError::execution(format!("create output {}: {error}", path.display()))
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| HarnessError::execution(format!("write {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_extension::EXTENSION_PACKAGE_SCHEMA_VERSION;

    const SOURCE: &str = "https://plugins.ternilo.dev/examples";
    const RHAI_PAYLOAD: &str = r"
fn echo_handler(context, arguments, settings) {
    #{ content: settings.prefix + arguments.text, is_error: false }
}
";

    fn manifest(runtime: &serde_json::Value) -> ExtensionManifest {
        let handler = if runtime["kind"] == "wasm-component" {
            "dispatch"
        } else {
            "echo_handler"
        };
        serde_json::from_value(serde_json::json!({
            "schema_version": EXTENSION_PACKAGE_SCHEMA_VERSION,
            "package_id": "dev.ternilo.cli-test",
            "version": "1.0.0",
            "source": SOURCE,
            "publisher_key_id": "cli-test-publisher",
            "payload_sha256": "0".repeat(64),
            "runtime": runtime,
            "config_schema": { "type": "object" },
            "contributions": {
                "tools": [{
                    "handler": handler,
                    "spec": {
                        "name": "cli_test_echo",
                        "description": "Echo a test value.",
                        "input_schema": { "type": "object" }
                    },
                    "output_schema": { "type": "object" },
                    "effect": "read_only"
                }],
                "prompt_sections": [],
                "skills": [],
                "hooks": [],
                "commands": [],
                "providers": []
            },
            "requested_capabilities": []
        }))
        .unwrap()
    }

    fn rhai_runtime() -> serde_json::Value {
        serde_json::json!({
            "kind": "rhai",
            "limits": {
                "max_operations": 1_000_000,
                "max_string_bytes": 1_048_576,
                "max_collection_items": 100_000,
                "max_call_levels": 64,
                "max_expr_depth": 64,
                "max_variables": 4_096,
                "max_functions": 256,
                "max_wall_ms": 60_000,
                "max_input_bytes": 1_048_576,
                "max_output_bytes": 4_194_304,
                "max_workspace_read_bytes": 2_097_152
            }
        })
    }

    fn wasm_runtime() -> serde_json::Value {
        serde_json::json!({
            "kind": "wasm-component",
            "world": "ternilo:extension/runtime@1.0.0",
            "limits": {
                "fuel": 1_000_000,
                "max_memory_bytes": 1_048_576,
                "max_input_bytes": 1_024,
                "max_output_bytes": 1_024,
                "max_workspace_read_bytes": 1_024
            }
        })
    }

    fn write_fixture_key(directory: &Path) -> (PathBuf, PathBuf) {
        let signing_key = SigningKey::from_bytes(&[17; 32]);
        let key_path = directory.join("publisher-key.json");
        write_json_new(
            &key_path,
            &PublisherKeyFile {
                schema_version: 1,
                key_id: "cli-test-publisher".to_owned(),
                private_key_base64: STANDARD.encode(signing_key.to_bytes()),
                public_key_base64: STANDARD.encode(signing_key.verifying_key().to_bytes()),
            },
            true,
        )
        .unwrap();
        let publisher_path = directory.join("publisher.json");
        export_publisher(&key_path, vec![SOURCE.to_owned()], &publisher_path).unwrap();
        (key_path, publisher_path)
    }

    #[test]
    fn signs_and_compile_verifies_a_rhai_extension() {
        let directory = tempfile::tempdir().unwrap();
        let manifest_path = directory.path().join("manifest.json");
        let payload_path = directory.path().join("extension.rhai");
        let bundle_path = directory.path().join("bundle.json");
        let (key_path, publisher_path) = write_fixture_key(directory.path());
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest(&rhai_runtime())).unwrap(),
        )
        .unwrap();
        std::fs::write(&payload_path, RHAI_PAYLOAD).unwrap();

        sign(&manifest_path, &payload_path, &key_path, &bundle_path).unwrap();
        let bundle: SignedExtensionBundle =
            read_json(&bundle_path, maximum_bundle_bytes()).unwrap();
        assert_eq!(
            bundle.payload,
            ExtensionPayload::Utf8(RHAI_PAYLOAD.to_owned())
        );
        assert_ne!(bundle.manifest.payload_sha256, "0".repeat(64));
        verify(&bundle_path, &publisher_path).unwrap();
    }

    #[test]
    fn wasm_payload_is_base64_and_reaches_component_compile_validation() {
        let directory = tempfile::tempdir().unwrap();
        let manifest_path = directory.path().join("manifest.json");
        let payload_path = directory.path().join("extension.component.wasm");
        let bundle_path = directory.path().join("bundle.json");
        let (key_path, publisher_path) = write_fixture_key(directory.path());
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest(&wasm_runtime())).unwrap(),
        )
        .unwrap();
        std::fs::write(&payload_path, b"component fixture").unwrap();

        sign(&manifest_path, &payload_path, &key_path, &bundle_path).unwrap();
        let bundle: SignedExtensionBundle =
            read_json(&bundle_path, maximum_bundle_bytes()).unwrap();
        assert!(matches!(bundle.payload, ExtensionPayload::Base64(_)));
        let error = verify(&bundle_path, &publisher_path).unwrap_err();
        assert!(
            error.message.contains("compile WASM Component extension"),
            "{error:?}"
        );
    }
}
