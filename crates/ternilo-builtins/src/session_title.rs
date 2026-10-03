use std::sync::Arc;

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use serde::{Deserialize, Serialize};
use ternilo_kernel::{
    DiscardModelOutput, HarnessPlugin, Models, ModelsClient, PluginFactory, PluginManifest,
    RunCancellation, SessionTitles, SessionTitlesProvider,
};
use ternilo_protocol::{HarnessError, MessageRole, ModelMessage, ModelRequest};

use crate::{EmptyConfig, factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.session_title.llm";
pub(crate) const SYSTEM_PROMPT: &str = "You name software-agent conversations. Return only one concise title in the user's language, without quotes, Markdown, a trailing period, or explanation. Prefer 2 to 8 words and describe the concrete task rather than the assistant.";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-session-title-llm@1",
        requires: [Models],
        provides: [SessionTitles],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/models@3"],
            provides: &["ternilo/session-titles@2"],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(SessionTitlePlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

struct SessionTitlePlugin;

impl HarnessPlugin for SessionTitlePlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let models = context
            .context()
            .service::<Models>()
            .expect("session title plugin declares Models");
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            let provider: Arc<dyn SessionTitlesProvider> = Arc::new(LlmSessionTitles { models });
            scope
                .provide::<SessionTitles>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide LLM session titles: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

struct LlmSessionTitles {
    models: ModelsClient,
}

impl SessionTitlesProvider for LlmSessionTitles {
    fn generate<'a>(
        &'a self,
        _: CallContext<()>,
        run_id: ternilo_protocol::RunId,
        request: String,
        answer: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let payload = serde_json::to_string(&TitleConversation {
                request: clipped(&request, 4_000),
                answer: clipped(&answer, 4_000),
            })
            .map_err(|error| {
                HarnessError::execution(format!("encode session title request: {error}"))
            })?;
            let response = self
                .models
                .complete(
                    ModelRequest {
                        run_id,
                        system_prompt: SYSTEM_PROMPT.to_owned(),
                        messages: vec![ModelMessage {
                            role: MessageRole::User,
                            content: payload,
                            reasoning_content: None,
                            provider_state: None,
                            attachments: Vec::new(),
                            tool_call_id: None,
                            tool_calls: Vec::new(),
                        }],
                        tools: Vec::new(),
                        step: 0,
                    },
                    Arc::new(DiscardModelOutput),
                    RunCancellation::new(),
                )
                .await?;
            if !response.tool_calls.is_empty() {
                return Err(HarnessError::execution(
                    "session title model returned tool calls",
                ));
            }
            normalize_title(&response.content)
        })
    }
}

#[derive(Deserialize, Serialize)]
struct TitleConversation {
    request: String,
    answer: String,
}

pub(crate) fn rule_title(payload: &str) -> Result<String, HarnessError> {
    let conversation: TitleConversation = serde_json::from_str(payload).map_err(|error| {
        HarnessError::execution(format!("decode deterministic title request: {error}"))
    })?;
    let request = conversation.request.trim();
    let candidate = request
        .strip_prefix('/')
        .and_then(|command| command.split_once(' ').map(|(_, content)| content))
        .filter(|content| !content.trim().is_empty())
        .unwrap_or(request);
    normalize_title(candidate)
}

fn clipped(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn normalize_title(value: &str) -> Result<String, HarnessError> {
    let value = value.trim().trim_matches('`').trim();
    let line = value
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let line = line
        .trim()
        .trim_start_matches('#')
        .trim()
        .strip_prefix("Title:")
        .or_else(|| line.trim().strip_prefix("标题："))
        .unwrap_or(line)
        .trim()
        .trim_matches(['"', '\'', '“', '”', '‘', '’'])
        .trim_end_matches(['.', '。'])
        .trim();
    let title = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = title.chars().take(80).collect::<String>();
    if title.is_empty() {
        Err(HarnessError::execution(
            "session title model returned an empty title",
        ))
    } else {
        Ok(title)
    }
}

#[cfg(test)]
mod tests {
    use super::{TitleConversation, normalize_title, rule_title};

    #[test]
    fn normalizes_model_wrappers_without_falling_back_to_the_prompt() {
        assert_eq!(
            normalize_title("```\nTitle: Rust relay diagnostics.\n```").unwrap(),
            "Rust relay diagnostics"
        );
    }

    #[test]
    fn deterministic_model_implements_the_same_title_contract() {
        let payload = serde_json::to_string(&TitleConversation {
            request: "/read README.md".to_owned(),
            answer: "Ternilo project guide".to_owned(),
        })
        .unwrap();
        assert_eq!(rule_title(&payload).unwrap(), "README.md");
    }
}
