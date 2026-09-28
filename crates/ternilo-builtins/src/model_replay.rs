use std::{future::Future, pin::Pin, sync::Arc};

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use ternilo_kernel::{
    HarnessPlugin, ModelOutput, Models, ModelsProvider, PluginFactory, PluginManifest,
    RunCancellation, SessionQueries, SessionQueriesClient, Sessions,
};
use ternilo_protocol::{
    HarnessError, ModelRequest, ModelResponse, SessionEventKind, SessionEventReadRequest, SessionId,
};
use tokio::sync::Mutex;

use crate::{factory as make_factory, model_request_digest, parse_config};

pub const KIND: &str = "ternilo.model.replay";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-replay-model@1",
        requires: [SessionQueries, Sessions],
        provides: [Models],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReplayConfig {
    source_session_id: String,
    #[serde(default)]
    strict_request_digest: bool,
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/session-queries@1", "ternilo/sessions@1"],
            provides: &["ternilo/models@3"],
        },
        |value| {
            let config: ReplayConfig = parse_config(value)?;
            let source = SessionId::new(config.source_session_id.as_str());
            source.validate()?;
            Ok(Arc::new(ReplayPlugin { config }))
        },
    )
    .with_config_schema::<ReplayConfig>()
}

struct ReplayPlugin {
    config: ReplayConfig,
}

impl HarnessPlugin for ReplayPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let queries = context
            .context()
            .service::<SessionQueries>()
            .expect("replay model declares SessionQueries");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("replay model declares Sessions");
        let route = context.context().clone();
        let scope = context.scope().clone();
        let config = self.config.clone();
        Activation::Once(Box::pin(async move {
            let consumed = sessions
                .events()
                .await
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::AssistantMessage { .. }))
                .count();
            let provider: Arc<dyn ModelsProvider> = Arc::new(ReplayModel {
                queries,
                source_session_id: SessionId::new(config.source_session_id),
                strict_request_digest: config.strict_request_digest,
                next_response: Mutex::new(consumed),
            });
            scope
                .provide::<Models>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide recorded replay model: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

struct ReplayModel {
    queries: SessionQueriesClient,
    source_session_id: SessionId,
    strict_request_digest: bool,
    next_response: Mutex<usize>,
}

impl ModelsProvider for ReplayModel {
    fn context_window<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Option<u64>> + Send + 'a>> {
        Box::pin(async { None })
    }

    fn complete<'a>(
        &'a self,
        _: CallContext<()>,
        request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            cancellation.check()?;
            let request_digest = model_request_digest(&request)?;
            let responses = self.source_responses().await?;
            let mut next = self.next_response.lock().await;
            let recorded = responses.get(*next).cloned().ok_or_else(|| {
                HarnessError::execution(format!(
                    "recorded replay source {} has no response at index {}",
                    self.source_session_id, *next
                ))
            })?;
            if self.strict_request_digest
                && recorded.request_digest.as_deref() != Some(request_digest.as_str())
            {
                return Err(HarnessError::execution(format!(
                    "recorded replay request digest mismatch at response index {}",
                    *next
                )));
            }
            *next += 1;
            drop(next);
            let mut response = recorded;
            response.replayed = true;
            response.attempts = 0;
            if !response.content.is_empty() {
                output.emit(response.content.clone()).await?;
            }
            cancellation.check()?;
            Ok(response)
        })
    }
}

impl ReplayModel {
    async fn source_responses(&self) -> Result<Vec<ModelResponse>, HarnessError> {
        let mut start_seq = 0_u64;
        let mut responses = Vec::new();
        loop {
            let events = self
                .queries
                .read_events(SessionEventReadRequest {
                    session_id: self.source_session_id.clone(),
                    start_seq,
                    limit: 200,
                })
                .await?;
            if events.is_empty() {
                break;
            }
            start_seq = events
                .last()
                .map_or(start_seq, |event| event.seq.saturating_add(1));
            let full_page = events.len() == 200;
            responses.extend(events.into_iter().filter_map(|event| match event.kind {
                SessionEventKind::AssistantMessage { response, .. } => Some(response),
                _ => None,
            }));
            if !full_page {
                break;
            }
        }
        Ok(responses)
    }
}
