use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use opentelemetry::{
    KeyValue,
    logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity},
};
use opentelemetry_otlp::{WithExportConfig as _, WithHttpConfig as _};
use opentelemetry_sdk::{
    Resource,
    logs::{SdkLogger, SdkLoggerProvider},
};
use serde_json::Value;
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, RunEnvironment, SessionTelemetry,
    SessionTelemetryBackendRegistration, SessionTelemetryRedactor, SessionTelemetrySink,
};
use ternilo_protocol::{
    HarnessError, SessionTelemetryRecord, SessionTelemetrySeverity, SessionTelemetrySharingStatus,
};

use crate::{factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.telemetry.otlp";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-telemetry-otlp@1",
        requires: [RunEnvironment, SessionTelemetry],
        provides: [],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/run-environment@1", "ternilo/session-telemetry@1"],
            provides: &[],
        },
        |value| {
            let config: TelemetryConfig = parse_config(value)?;
            if config.timeout_ms == 0 {
                return Err(HarnessError::composition(
                    "telemetry timeout_ms must be positive",
                ));
            }
            if config.service_name.trim().is_empty() {
                return Err(HarnessError::composition(
                    "telemetry service_name must not be empty",
                ));
            }
            Ok(Arc::new(TelemetryPlugin { config }))
        },
    )
    .with_config_schema::<TelemetryConfig>()
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TelemetryMode {
    #[default]
    Disabled,
    FeedbackOnly,
    Full,
}

impl From<TelemetryMode> for SessionTelemetrySharingStatus {
    fn from(value: TelemetryMode) -> Self {
        match value {
            TelemetryMode::Disabled => Self::Disabled,
            TelemetryMode::FeedbackOnly => Self::FeedbackOnly,
            TelemetryMode::Full => Self::Full,
        }
    }
}

#[derive(Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TelemetryConfig {
    #[serde(default)]
    mode: TelemetryMode,
    /// OTLP/HTTP logs endpoint. When omitted, the standard OpenTelemetry
    /// environment variables and SDK default are used.
    endpoint: Option<String>,
    #[serde(default = "default_service_name")]
    service_name: String,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    /// Non-secret OTLP headers.
    #[serde(default)]
    headers: BTreeMap<String, String>,
    /// Header name -> host credential reference. Secret values are resolved at
    /// activation and never enter the profile, catalog, session log, or relay.
    #[serde(default)]
    credential_headers: BTreeMap<String, String>,
}

/// Trusted-host configuration for exporting one durable Cloud occurrence.
/// Secret header values belong only in the Worker parent that constructs this
/// value; they are never serialized into a `RunSpec` or child protocol frame.
#[derive(Clone, Debug)]
pub struct OtlpTelemetryExporterConfig {
    pub endpoint: String,
    pub service_name: String,
    pub timeout: Duration,
    pub headers: BTreeMap<String, String>,
}

/// Export a durable batch and wait for the OTLP provider to flush it before
/// returning. `occurrence_id` is attached to every record so a collector can
/// recognize an at-least-once retry after a lost database acknowledgement.
pub async fn export_otlp_telemetry_occurrence(
    config: &OtlpTelemetryExporterConfig,
    occurrence_id: &str,
    records: Vec<SessionTelemetryRecord>,
) -> Result<(), HarnessError> {
    if config.endpoint.trim().is_empty()
        || config.service_name.trim().is_empty()
        || config.timeout.is_zero()
        || occurrence_id.trim().is_empty()
    {
        return Err(HarnessError::invalid(
            "OTLP endpoint, service name, timeout, and occurrence id are required",
        ));
    }
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(config.endpoint.clone())
        .with_timeout(config.timeout)
        .with_headers(config.headers.clone().into_iter().collect())
        .build()
        .map_err(|error| HarnessError::execution(format!("build OTLP exporter: {error}")))?;
    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();
    let provider = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build();
    let logger = provider.logger("ternilo.session");
    for record in records {
        emit_otlp_record(&logger, record, Some(occurrence_id));
    }
    tokio::task::spawn_blocking(move || provider.shutdown())
        .await
        .map_err(|error| HarnessError::execution(format!("join OTLP exporter: {error}")))?
        .map_err(|error| HarnessError::execution(format!("flush OTLP exporter: {error}")))
}

fn default_service_name() -> String {
    "ternilo".to_owned()
}

const fn default_timeout_ms() -> u64 {
    10_000
}

struct TelemetryPlugin {
    config: TelemetryConfig,
}

impl HarnessPlugin for TelemetryPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let telemetry = context
            .context()
            .service::<SessionTelemetry>()
            .expect("telemetry plugin declares SessionTelemetry");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("telemetry plugin declares RunEnvironment");
        let config = self.config.clone();
        Activation::Once(Box::pin(async move {
            if matches!(config.mode, TelemetryMode::Disabled) {
                return Ok(None);
            }

            let mut headers = config.headers.into_iter().collect::<HashMap<_, _>>();
            for (header, credential) in config.credential_headers {
                let value = environment
                    .resolve_secret(credential.clone())
                    .await
                    .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?
                    .ok_or_else(|| {
                        linorun_core::ActivationFailure::user(format!(
                            "telemetry credential {credential:?} is not configured"
                        ))
                    })?;
                headers.insert(header, value);
            }

            let mut exporter = opentelemetry_otlp::LogExporter::builder()
                .with_http()
                .with_timeout(Duration::from_millis(config.timeout_ms))
                .with_headers(headers);
            if let Some(endpoint) = config.endpoint {
                exporter = exporter.with_endpoint(endpoint);
            }
            let exporter = exporter.build().map_err(|error| {
                linorun_core::ActivationFailure::user(format!(
                    "build OTLP telemetry exporter: {error}"
                ))
            })?;
            let resource = Resource::builder()
                .with_service_name(config.service_name)
                .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
                .build();
            let provider = SdkLoggerProvider::builder()
                .with_resource(resource)
                .with_batch_exporter(exporter)
                .build();
            let logger = provider.logger("ternilo.session");
            let sink: Arc<dyn SessionTelemetrySink> = Arc::new(OtlpSink { provider, logger });
            let redactor = telemetry
                .register_redactor(Arc::new(StructuralSecretRedactor))
                .await;
            let backend = match telemetry
                .register_backend(SessionTelemetryBackendRegistration {
                    sharing: config.mode.into(),
                    sink,
                })
                .await
            {
                Ok(registration) => registration,
                Err(error) => {
                    let _ = telemetry.unregister_redactor(redactor).await;
                    return Err(linorun_core::ActivationFailure::user(error.to_string()));
                }
            };
            Ok(Some(effect::inverse(move || async move {
                telemetry
                    .unregister_backend(backend)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))?;
                telemetry
                    .unregister_redactor(redactor)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

struct OtlpSink {
    provider: SdkLoggerProvider,
    logger: SdkLogger,
}

impl SessionTelemetrySink for OtlpSink {
    fn emit(&self, record: SessionTelemetryRecord) {
        emit_otlp_record(&self.logger, record, None);
    }

    fn shutdown<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let provider = self.provider.clone();
        Box::pin(async move {
            let _ = tokio::task::spawn_blocking(move || provider.shutdown()).await;
        })
    }
}

fn emit_otlp_record(
    logger: &SdkLogger,
    record: SessionTelemetryRecord,
    occurrence_id: Option<&str>,
) {
    let mut output = logger.create_log_record();
    let (severity, severity_text) = match record.severity {
        SessionTelemetrySeverity::Info => (Severity::Info, "INFO"),
        SessionTelemetrySeverity::Warn => (Severity::Warn, "WARN"),
        SessionTelemetrySeverity::Error => (Severity::Error, "ERROR"),
    };
    output.set_severity_number(severity);
    output.set_severity_text(severity_text);
    output.set_timestamp(UNIX_EPOCH + Duration::from_millis(record.time_ms));
    output.set_body(AnyValue::from(record.body.to_string()));
    for (key, value) in record.attributes {
        output.add_attribute(key, telemetry_value(value));
    }
    if let Some(occurrence_id) = occurrence_id {
        output.add_attribute("ternilo.telemetry.occurrence_id", occurrence_id.to_owned());
    }
    logger.emit(output);
}

fn telemetry_value(value: Value) -> AnyValue {
    match value {
        Value::Bool(value) => AnyValue::from(value),
        Value::Number(value) => value
            .as_i64()
            .map_or_else(|| AnyValue::from(value.to_string()), AnyValue::from),
        Value::String(value) => AnyValue::from(value),
        other => AnyValue::from(other.to_string()),
    }
}

struct StructuralSecretRedactor;

impl SessionTelemetryRedactor for StructuralSecretRedactor {
    fn redact(&self, record: SessionTelemetryRecord) -> Option<SessionTelemetryRecord> {
        Some(redact_session_telemetry_record(record))
    }
}

/// Apply the built-in structural secret redaction to a detached record.
#[must_use]
pub fn redact_session_telemetry_record(
    mut record: SessionTelemetryRecord,
) -> SessionTelemetryRecord {
    redact_value(None, &mut record.body);
    record
}

fn redact_value(key: Option<&str>, value: &mut Value) {
    if key.is_some_and(sensitive_key) {
        *value = Value::String("[redacted]".to_owned());
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                redact_value(None, item);
            }
        }
        Value::Object(fields) => {
            for (field, value) in fields {
                redact_value(Some(field), value);
            }
        }
        _ => {}
    }
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "authorization",
        "apikey",
        "accesstoken",
        "refreshtoken",
        "password",
        "passwd",
        "credential",
        "clientsecret",
        "cookie",
        "setcookie",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use serde_json::json;

    use super::*;
    use ternilo_kernel::{HarnessSession, HostEnvironment, HostPolicy, HostSessionTelemetry};
    use ternilo_protocol::{
        AgentId, RunId, RunLimits, SessionEvent, SessionEventKind, SessionId, SessionIdentity,
        SessionTelemetryChannel, TenantId, UserId,
    };

    async fn otlp_fixture() -> (SocketAddr, tokio::task::JoinHandle<Vec<u8>>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind OTLP fixture");
        let address = listener.local_addr().expect("OTLP fixture address");
        let fixture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept OTLP request");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 8 * 1024];
            loop {
                let size = stream.read(&mut buffer).await.expect("read OTLP request");
                if size == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..size]);
                let Some(header_start) =
                    request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let header_end = header_start + 4;
                let headers = String::from_utf8_lossy(&request[..header_start]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(name, value)| {
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + content_length {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await
                .expect("write OTLP response");
            request
        });
        (address, fixture)
    }

    async fn otlp_retry_fixture() -> (SocketAddr, tokio::task::JoinHandle<Vec<Vec<u8>>>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind retry OTLP fixture");
        let address = listener.local_addr().expect("retry OTLP fixture address");
        let fixture = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.expect("accept OTLP retry");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 8 * 1024];
                loop {
                    let size = stream.read(&mut buffer).await.expect("read OTLP retry");
                    if size == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..size]);
                    let Some(header_start) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let header_end = header_start + 4;
                    let headers = String::from_utf8_lossy(&request[..header_start]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':').and_then(|(name, value)| {
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await
                    .expect("write OTLP retry response");
                requests.push(request);
            }
            requests
        });
        (address, fixture)
    }

    async fn collect_fixture_request(fixture: tokio::task::JoinHandle<Vec<u8>>) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(10), fixture)
            .await
            .expect("OTLP fixture timeout")
            .expect("OTLP fixture join")
    }

    fn test_identity(session_id: &str) -> SessionIdentity {
        SessionIdentity {
            tenant_id: TenantId::new("local"),
            user_id: UserId::new("user"),
            agent_id: AgentId::new("agent"),
            session_id: SessionId::new(session_id),
        }
    }

    #[test]
    fn structural_redaction_changes_only_the_outbound_copy() {
        let original = json!({
            "type": "tool_call_started",
            "call": {
                "arguments": {
                    "api_key": "secret",
                    "nested": { "Authorization": "Bearer hidden" },
                    "query": "public"
                }
            }
        });
        let record = SessionTelemetryRecord {
            channel: SessionTelemetryChannel::Ledger,
            time_ms: 1,
            severity: SessionTelemetrySeverity::Info,
            attributes: BTreeMap::new(),
            body: original.clone(),
        };
        let redacted = StructuralSecretRedactor.redact(record).expect("retained");
        assert_eq!(redacted.body["call"]["arguments"]["api_key"], "[redacted]");
        assert_eq!(
            redacted.body["call"]["arguments"]["nested"]["Authorization"],
            "[redacted]"
        );
        assert_eq!(redacted.body["call"]["arguments"]["query"], "public");
        assert_eq!(original["call"]["arguments"]["api_key"], "secret");
    }

    #[test]
    fn sensitive_key_normalization_handles_common_spellings() {
        for key in ["api_key", "apiKey", "ACCESS-TOKEN", "client_secret"] {
            assert!(sensitive_key(key), "{key}");
        }
        assert!(!sensitive_key("token_count"));
    }

    #[test]
    fn event_json_remains_serializable_for_the_otlp_body() {
        let event = SessionEvent {
            seq: 0,
            occurred_at_ms: 1,
            run_id: RunId::new("run"),
            kind: SessionEventKind::TurnFinished {
                answer: "done".to_owned(),
                finish_reason: ternilo_protocol::TurnFinishReason::Completed,
            },
        };
        assert!(serde_json::to_value(event.kind).is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn durable_retry_keeps_the_same_occurrence_id_at_the_otlp_receiver() {
        let (address, fixture) = otlp_retry_fixture().await;
        let config = OtlpTelemetryExporterConfig {
            endpoint: format!("http://{address}/v1/logs"),
            service_name: "ternilo-cloud-retry-test".to_owned(),
            timeout: Duration::from_secs(5),
            headers: BTreeMap::new(),
        };
        let occurrence_id = "tel_retry_occurrence_1";
        let record = SessionTelemetryRecord {
            channel: SessionTelemetryChannel::Ledger,
            time_ms: 42,
            severity: SessionTelemetrySeverity::Info,
            attributes: BTreeMap::new(),
            body: json!({"type": "turn_started"}),
        };
        export_otlp_telemetry_occurrence(&config, occurrence_id, vec![record.clone()])
            .await
            .expect("first occurrence export");
        export_otlp_telemetry_occurrence(&config, occurrence_id, vec![record])
            .await
            .expect("retry occurrence export");
        let requests = tokio::time::timeout(Duration::from_secs(10), fixture)
            .await
            .expect("OTLP retry fixture timeout")
            .expect("OTLP retry fixture join");
        assert_eq!(requests.len(), 2);
        for request in requests {
            let occurrences = request
                .windows(occurrence_id.len())
                .filter(|window| *window == occurrence_id.as_bytes())
                .count();
            assert_eq!(occurrences, 1, "one occurrence id per retry payload");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicit_full_mode_exports_otlp_and_discloses_its_policy() {
        let (address, fixture) = otlp_fixture().await;

        let mut profile = crate::local_profile();
        let telemetry_entry = profile
            .plugins
            .iter_mut()
            .find(|entry| entry.kind == KIND)
            .expect("telemetry row");
        telemetry_entry.config = json!({
            "mode": "full",
            "endpoint": format!("http://{address}/v1/logs"),
            "service_name": "ternilo-test",
            "timeout_ms": 5000
        });
        let telemetry = Arc::new(HostSessionTelemetry::default());
        let identity = test_identity("session");
        let environment =
            HostEnvironment::memory(identity, None, HostPolicy::local(RunLimits::default()))
                .with_session_telemetry(Arc::clone(&telemetry));
        let harness =
            HarnessSession::boot(&crate::catalog().expect("catalog"), &profile, environment)
                .await
                .expect("boot harness");
        assert_eq!(
            telemetry.sharing_status(),
            SessionTelemetrySharingStatus::Full
        );
        harness
            .run(RunId::new("run"), "hello")
            .await
            .expect("run turn");
        harness.shutdown().await.expect("shutdown harness");
        assert_eq!(
            telemetry.sharing_status(),
            SessionTelemetrySharingStatus::Disabled
        );
        let request = collect_fixture_request(fixture).await;
        let header_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP request headers");
        let headers = String::from_utf8_lossy(&request[..header_end]);
        assert!(headers.starts_with("POST /v1/logs HTTP/1.1"));
        assert!(
            request.len() > header_end + 4,
            "OTLP protobuf body is empty"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn feedback_only_exports_each_committed_suffix_once_to_otlp() {
        let (address, fixture) = otlp_fixture().await;
        let mut profile = crate::local_profile();
        let telemetry_entry = profile
            .plugins
            .iter_mut()
            .find(|entry| entry.kind == KIND)
            .expect("telemetry row");
        telemetry_entry.config = json!({
            "mode": "feedback_only",
            "endpoint": format!("http://{address}/v1/logs"),
            "service_name": "ternilo-feedback-test",
            "timeout_ms": 5000
        });
        let telemetry = Arc::new(HostSessionTelemetry::default());
        let environment = HostEnvironment::memory(
            test_identity("feedback-session"),
            None,
            HostPolicy::local(RunLimits::default()),
        )
        .with_session_telemetry(Arc::clone(&telemetry));
        let harness =
            HarnessSession::boot(&crate::catalog().expect("catalog"), &profile, environment)
                .await
                .expect("boot harness");
        assert_eq!(
            telemetry.sharing_status(),
            SessionTelemetrySharingStatus::FeedbackOnly
        );

        let run_id = RunId::new("feedback-run");
        harness
            .append_event(run_id.clone(), SessionEventKind::TurnStarted)
            .await
            .expect("append turn start");
        harness
            .append_event(
                run_id.clone(),
                SessionEventKind::CommandStarted {
                    command_id: "prefix-once-sentinel".to_owned(),
                    command_name: "first".to_owned(),
                },
            )
            .await
            .expect("append first prefix event");
        harness
            .append_event(
                run_id.clone(),
                SessionEventKind::FeedbackSubmitted {
                    command_id: "feedback-one".to_owned(),
                    text: "first release".to_owned(),
                },
            )
            .await
            .expect("append first feedback");
        harness
            .append_event(
                run_id.clone(),
                SessionEventKind::CommandStarted {
                    command_id: "second-suffix-sentinel".to_owned(),
                    command_name: "second".to_owned(),
                },
            )
            .await
            .expect("append second suffix event");
        harness
            .append_event(
                run_id,
                SessionEventKind::FeedbackSubmitted {
                    command_id: "feedback-two".to_owned(),
                    text: "second release".to_owned(),
                },
            )
            .await
            .expect("append second feedback");
        harness.shutdown().await.expect("shutdown harness");

        let request = collect_fixture_request(fixture).await;
        let body = String::from_utf8_lossy(&request);
        for sentinel in [
            "prefix-once-sentinel",
            "second-suffix-sentinel",
            "feedback-one",
            "feedback-two",
        ] {
            assert_eq!(
                body.matches(sentinel).count(),
                1,
                "OTLP payload must contain {sentinel:?} exactly once"
            );
        }
    }
}
