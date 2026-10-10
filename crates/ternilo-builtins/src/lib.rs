#![forbid(unsafe_code)]

mod model_gateway;
pub use model_gateway::{
    BROKERED_MODEL_KIND, BrokeredModelConfig, model_gateway_factory, profile_model_snapshot,
    read_model_gateway_response,
};

mod agent;
mod agent_goal;
mod agent_team;
mod context;
mod contributions;
mod extensions;
#[cfg(test)]
mod goal_execution_tests;
mod hooks;
mod hosted_tools;
pub use hosted_tools::apply_hosted_web_tools;
mod instructions;
mod jobs;
mod lsp;
mod mcp;
mod model;
mod model_discovery;
mod model_replay;
mod model_rule;
mod process_group;
pub mod process_supervision;
mod projection;
mod prompt;
mod schedule;
mod session;
mod session_query;
mod session_title;
mod skill;
#[cfg(all(test, unix))]
mod stdio_test;
mod subagent;
mod subagent_acp;
mod telemetry;
mod terminal;
mod tools;
mod web;
mod web_search;
mod workflow;
mod workspace_tools;

use std::{fmt::Write as _, sync::Arc};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use ternilo_kernel::{Catalog, HarnessPlugin, PluginFactory, PluginManifest};
use ternilo_protocol::{HarnessError, PluginEntry, Profile};

pub use agent_team::KIND as AGENT_TEAM_TOOLS_KIND;
pub use context::KIND as CONTEXT_KIND;
pub use contributions::PROMPT_SECTION_KIND;
pub use extensions::KIND as RUNTIME_EXTENSION_TOOLS_KIND;
pub use hooks::{CLAUDE_CODE_KIND as CLAUDE_CODE_HOOKS_KIND, CODEX_KIND as CODEX_HOOKS_KIND};
pub use instructions::KIND as INSTRUCTIONS_KIND;
pub use jobs::KIND as JOB_TOOLS_KIND;
pub use lsp::KIND as LSP_STDIO_KIND;
pub use model::{
    ModelAttemptObserver, ModelAttemptReport, ProviderModelRoute, complete_provider_model,
    normalize_provider_usage,
};
pub use model_discovery::{discover_provider_models, parse_openai_model_catalog};
mod provider_http;
pub use model_replay::KIND as MODEL_REPLAY_KIND;
pub use model_rule::{RuleCommand, parse_rule_command, rule_command_name};
pub use projection::{session_stats, stats_unit};
pub use provider_http::{apply_native_reasoning, provider_model_endpoint, provider_request};
pub use schedule::{KIND as SCHEDULE_TOOL_KIND, has_pending_schedules, pending_schedules};
pub use session::recovery_events as interrupted_history_events;
pub use session_query::KIND as SESSION_QUERY_TOOLS_KIND;
pub use session_title::KIND as SESSION_TITLE_KIND;
pub use skill::{
    FILESYSTEM_KIND as FILESYSTEM_SKILL_KIND, REGISTRY_KIND as SKILL_REGISTRY_KIND,
    TOOL_KIND as SKILL_TOOL_KIND, prepare_skill_invocation, render_skill_content,
};
pub use subagent::KIND as SUBAGENT_KIND;
pub use subagent_acp::KIND as ACP_SUBAGENT_KIND;
pub use telemetry::{
    KIND as OTLP_TELEMETRY_KIND, OtlpTelemetryExporterConfig, export_otlp_telemetry_occurrence,
    redact_session_telemetry_record,
};
pub use terminal::KIND as TERMINAL_TOOLS_KIND;
pub use web::KIND as WEB_FETCH_KIND;
pub use web_search::KIND as WEB_SEARCH_SEARXNG_KIND;
pub use web_search::{BRAVE_KIND as WEB_SEARCH_BRAVE_KIND, TAVILY_KIND as WEB_SEARCH_TAVILY_KIND};
pub use workflow::{ENGINE_KIND as WORKFLOW_ENGINE_KIND, TOOL_KIND as WORKFLOW_TOOL_KIND};
pub use workspace_tools::{ASK_USER_TOOL_KIND, FILE_TOOLS_KIND, PLAN_TOOL_KIND, SHELL_TOOL_KIND};

pub const CATALOG_REVISION: &str = "ternilo-builtins-v19";

const MAX_PROVIDER_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

const fn effective_count_limit(plugin_limit: u32, host_limit: u32) -> u32 {
    match (plugin_limit, host_limit) {
        (0, host) => host,
        (plugin, 0) => plugin,
        (plugin, host) => {
            if plugin < host {
                plugin
            } else {
                host
            }
        }
    }
}

fn model_request_digest(request: &ternilo_protocol::ModelRequest) -> Result<String, HarnessError> {
    let bytes = serde_json::to_vec(request)
        .map_err(|error| HarnessError::execution(format!("serialize model request: {error}")))?;
    Ok(Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut digest, byte| {
            write!(digest, "{byte:02x}").expect("writing to a String cannot fail");
            digest
        }))
}

pub fn catalog() -> Result<Catalog, HarnessError> {
    let mut catalog = Catalog::new(CATALOG_REVISION);
    register(&mut catalog)?;
    Ok(catalog)
}

pub fn register(catalog: &mut Catalog) -> Result<(), HarnessError> {
    for factory in [
        session::factory(),
        session_title::factory(),
        session_query::factory(),
        prompt::factory(),
        tools::factory(),
        agent_team::factory(),
        hooks::registry_factory(),
        hooks::claude_code_factory(),
        hooks::codex_factory(),
        contributions::system_prompt_factory(),
        contributions::identity_prompt_factory(),
        contributions::prompt_section_factory(),
        instructions::factory(),
        extensions::factory(),
        schedule::factory(),
        context::factory(),
        workspace_tools::file_tools_factory(),
        workspace_tools::shell_tool_factory(),
        workspace_tools::ask_user_tool_factory(),
        workspace_tools::plan_tool_factory(),
        skill::registry_factory(),
        skill::filesystem_factory(),
        skill::tool_factory(),
        subagent::factory(),
        subagent_acp::factory(),
        terminal::factory(),
        telemetry::factory(),
        mcp::factory(),
        jobs::factory(),
        lsp::factory(),
        web::factory(),
        web_search::factory(),
        web_search::brave_factory(),
        web_search::tavily_factory(),
        workflow::engine_factory(),
        workflow::factory(),
        model_rule::factory(),
        model::factory(),
        model_replay::factory(),
        agent::factory(),
    ] {
        catalog.register(factory)?;
    }
    Ok(())
}

#[must_use]
pub fn local_profile() -> Profile {
    Profile {
        plugins: vec![
            entry("session", session::KIND, json!({})),
            entry("session-title", session_title::KIND, json!({})),
            entry("prompt-registry", prompt::KIND, json!({})),
            entry("tool-registry", tools::KIND, json!({})),
            entry("hook-registry", hooks::REGISTRY_KIND, json!({})),
            entry(
                "system-prompt",
                contributions::SYSTEM_PROMPT_KIND,
                json!({
                    "content": "You are Ternilo, a concise and capable local software agent."
                }),
            ),
            entry(
                "identity-prompt",
                contributions::IDENTITY_PROMPT_KIND,
                json!({}),
            ),
            entry("model", model_rule::KIND, json!({ "prefix": "ternilo: " })),
            entry("context", context::KIND, json!({})),
            entry(
                "session-telemetry",
                telemetry::KIND,
                json!({ "mode": "disabled" }),
            ),
            entry("subagents", subagent::KIND, json!({})),
            entry("workflow-engine", workflow::ENGINE_KIND, json!({})),
            entry("workflow-tool", workflow::TOOL_KIND, json!({})),
            entry("runtime-extension-tools", extensions::KIND, json!({})),
            entry("agent-loop", agent::KIND, json!({})),
        ],
    }
}

fn entry(id: &str, kind: &str, config: Value) -> PluginEntry {
    PluginEntry {
        id: id.to_owned(),
        kind: kind.to_owned(),
        enabled: true,
        config,
    }
}

fn factory(
    manifest: PluginManifest,
    build: fn(Value) -> Result<Arc<dyn HarnessPlugin>, HarnessError>,
) -> PluginFactory {
    let description = plugin_description(manifest.kind);
    PluginFactory::new(manifest, build).with_description(description)
}

fn plugin_description(kind: &str) -> &'static str {
    match kind {
        "ternilo.session.log" => "保存 append-only 会话事件，并从事件构建模型历史。",
        "ternilo.session_title.llm" => "首次成功运行后用当前会话模型生成简洁标题。",
        "ternilo.tool.session_query" => "搜索、读取和追踪当前节点上的持久会话。",
        "ternilo.prompt.registry" => "按顺序组合由其他插件贡献的系统提示词。",
        "ternilo.tools.registry" => "注册工具并统一执行权限、审批、超时与副作用检查。",
        "ternilo.tools.agent_team" => "管理当前 Agent Team 的共享任务和成员邮箱。",
        "ternilo.hooks.registry" => "在会话、提示、工具和停止边界运行有序 Hooks。",
        "ternilo.hooks.claude_code" => "运行 Claude Code 配置中的同步 command Hooks。",
        "ternilo.hooks.codex" => "运行 Codex 配置中的同步 command Hooks。",
        "ternilo.prompt.system" => "提供 Agent 的基础系统提示词。",
        "ternilo.prompt.identity" => "向模型说明当前用户、会话和工作区身份。",
        "ternilo.prompt.section" => "向系统提示词追加一个可排序的自定义段落。",
        "ternilo.prompt.workspace_instructions" => "发现并注入工作区级说明文件。",
        "ternilo.tools.runtime_extensions" => "检查并经审批管理已安装的签名运行时扩展。",
        "ternilo.tool.schedule" => "创建、查看和取消持久定时任务。",
        "ternilo.context.compaction" => "在上下文接近上限时压缩较早的会话历史。",
        "ternilo.tools.files" => "向模型提供工作区文件读取、写入、替换、glob 与 grep。",
        "ternilo.tool.shell" => "通过宿主 sandbox 执行一次性 shell 命令。",
        "ternilo.tool.ask_user" => "暂停运行并向用户提出可恢复的问题或审批。",
        "ternilo.tool.plan" => "维护计划、todo、goal 和需用户审阅的 Plan 退出。",
        "ternilo.skills.registry" => "组合多个 Skill provider 的摘要与按需加载服务。",
        "ternilo.skills.filesystem" => "从项目、用户和自定义目录发现 SKILL.md。",
        "ternilo.tools.skills" => "让模型列出并按需加载可用 Skills。",
        "ternilo.subagents.in_process" => "提供内置后台子代理和 provider registry。",
        "ternilo.subagents.acp" => "把外部 ACP v1 进程注册为子代理 provider。",
        "ternilo.tools.terminal" => "向模型提供持久终端的创建、输入、读取和关闭操作。",
        "ternilo.telemetry.otlp" => "按显式模式复制经遮蔽的会话事件到 OTLP collector。",
        "ternilo.mcp.stdio" => "启动 MCP stdio server，并把远端工具注册到共享工具链。",
        "ternilo.tools.jobs" => "启动、查看和取消后台 shell job。",
        "ternilo.lsp.stdio" => "启动语言服务器，并提供受审批的 JSON-RPC 工具。",
        "ternilo.tool.web_fetch" => "抓取有界的公开 HTTP(S) 页面内容。",
        "ternilo.web.search.searxng" => "通过配置的 SearXNG endpoint 搜索网页。",
        "ternilo.web.search.brave" => "使用执行电脑的凭据通过 Brave Search 搜索网页。",
        "ternilo.web.search.tavily" => "使用执行电脑的凭据通过 Tavily Search 搜索网页。",
        "ternilo.workflow.rhai" => "用受限 Rhai runtime 编排并行和流水线子代理。",
        "ternilo.tools.workflow" => "向模型公开需审批的 Workflow 工具。",
        "ternilo.model.rule" => "无需联网的确定性规则模型，用于验证 Harness 链路。",
        "ternilo.model.openai_compatible" => "调用 OpenAI-compatible SSE 模型端点。",
        "ternilo.model.replay" => "按已记录的模型交互重放确定性响应。",
        "ternilo.agent.react" => "执行模型、工具、结果迭代的 ReAct agent loop。",
        _ => "Ternilo 内置插件。",
    }
}

fn parse_config<T>(value: Value) -> Result<T, HarnessError>
where
    T: DeserializeOwned,
{
    let value = if value.is_null() { json!({}) } else { value };
    serde_json::from_value(value)
        .map_err(|error| HarnessError::composition(format!("invalid plugin config: {error}")))
}

async fn read_provider_response(
    mut response: reqwest::Response,
    provider: &str,
) -> Result<Vec<u8>, HarnessError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES as u64)
    {
        return Err(HarnessError::execution(format!(
            "{provider} response exceeds {MAX_PROVIDER_RESPONSE_BYTES} bytes"
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| HarnessError::execution(format!("read {provider} response: {error}")))?
    {
        if chunk.len() > MAX_PROVIDER_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(HarnessError::execution(format!(
                "{provider} response exceeds {MAX_PROVIDER_RESPONSE_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyConfig {}

#[cfg(test)]
mod tool_history_tests;

#[cfg(test)]
mod run_limit_tests;

#[cfg(test)]
mod manual_goal_tests;

#[cfg(test)]
mod tests {
    use ternilo_kernel::{HarnessSession, HostPolicy};
    use ternilo_protocol::{
        AgentId, RunId, RunLimits, SessionId, SessionIdentity, TenantId, UserId,
    };

    use super::*;

    async fn mock_completion_server() -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 32 * 1024];
            let size = stream.read(&mut request).await.unwrap();
            request.truncate(size);
            let body =
                r#"{"choices":[{"message":{"content":"mock model answer","tool_calls":[]}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (format!("http://{address}/v1"), task)
    }

    async fn mock_streaming_server() -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 32 * 1024];
            let size = stream.read(&mut request).await.unwrap();
            request.truncate(size);
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"consider \"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"this\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"streamed \"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":7,\"completion_tokens_details\":{\"reasoning_tokens\":3}}}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (format!("http://{address}/v1"), task)
    }

    async fn mock_retry_server() -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            for attempt in 1..=2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 32 * 1024];
                let _ = stream.read(&mut request).await.unwrap();
                let (status, body) = if attempt == 1 {
                    ("503 Service Unavailable", r#"{"error":"busy"}"#)
                } else {
                    (
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"after retry","tool_calls":[]}}]}"#,
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}/v1"), task)
    }

    fn identity() -> SessionIdentity {
        SessionIdentity {
            tenant_id: TenantId::new("local"),
            user_id: UserId::new("tester"),
            agent_id: AgentId::new("default"),
            session_id: SessionId::new("session-1"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn builtins_run_multiple_turns_in_one_session() {
        let catalog = catalog().unwrap();
        let environment = ternilo_kernel::HostEnvironment::memory(
            identity(),
            None,
            HostPolicy::local(RunLimits::default()),
        );
        let harness = HarnessSession::boot(&catalog, &local_profile(), environment)
            .await
            .unwrap();

        let first = harness.run(RunId::new("run-1"), "hello").await.unwrap();
        assert_eq!(first.answer, "ternilo: hello");
        assert!(harness.run(RunId::new("run-1"), "duplicate").await.is_err());
        let second = harness.run(RunId::new("run-2"), "/agents").await.unwrap();
        assert_eq!(second.answer, "[]");
        assert_eq!(second.tool_calls, 1);
        assert!(harness.events().await.len() > second.events.len());

        harness.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn host_tool_denial_cannot_be_overridden_by_plugins() {
        let catalog = catalog().unwrap();
        let mut denied = std::collections::BTreeSet::new();
        denied.insert("list_agents".to_owned());
        let environment = ternilo_kernel::HostEnvironment::memory(
            identity(),
            None,
            HostPolicy {
                limits: RunLimits::default(),
                denied_tools: denied,
                permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
                allow_mutating_tools: true,
            },
        );
        let harness = HarnessSession::boot(&catalog, &local_profile(), environment)
            .await
            .unwrap();

        let result = harness.run(RunId::new("run-1"), "/agents").await;
        assert!(result.is_ok(), "tool errors are model-visible results");
        let outcome = result.unwrap();
        assert!(outcome.answer.contains("denied by host policy"));
        harness.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn openai_compatible_provider_uses_the_shared_model_contract() {
        let (base_url, request) = mock_completion_server().await;
        let mut profile = local_profile();
        let model = profile
            .plugins
            .iter_mut()
            .find(|entry| entry.id == "model")
            .unwrap();
        model.kind = model::KIND.to_owned();
        model.config = json!({
            "provider": "mock-provider",
            "base_url": base_url,
            "model": "mock-model",
            "timeout_ms": 5000
        });
        let harness = HarnessSession::boot(
            &catalog().unwrap(),
            &profile,
            ternilo_kernel::HostEnvironment::memory(
                identity(),
                None,
                HostPolicy::local(RunLimits::default()),
            ),
        )
        .await
        .unwrap();
        let outcome = harness.run(RunId::new("run-http"), "hello").await.unwrap();
        assert_eq!(outcome.answer, "mock model answer");
        let request = request.await.unwrap();
        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(request.contains("mock-model"));
        let body: serde_json::Value = serde_json::from_str(
            request
                .split_once("\r\n\r\n")
                .expect("HTTP request contains a body")
                .1,
        )
        .unwrap();
        let request_system_prompt = body["messages"][0]["content"].as_str().unwrap();
        let recorded_system_prompt = outcome
            .events
            .iter()
            .find_map(|event| match &event.kind {
                ternilo_protocol::SessionEventKind::ModelRequestStarted {
                    system_prompt, ..
                } => Some(system_prompt.as_str()),
                _ => None,
            })
            .unwrap();
        assert_eq!(recorded_system_prompt, request_system_prompt);
        harness.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn openai_compatible_provider_streams_into_ordered_session_events() {
        let (base_url, request) = mock_streaming_server().await;
        let mut profile = local_profile();
        let model = profile
            .plugins
            .iter_mut()
            .find(|entry| entry.id == "model")
            .unwrap();
        model.kind = model::KIND.to_owned();
        model.config = json!({
            "provider": "mock-provider",
            "base_url": base_url,
            "model": "mock-stream-model",
            "timeout_ms": 5000
        });
        let harness = HarnessSession::boot(
            &catalog().unwrap(),
            &profile,
            ternilo_kernel::HostEnvironment::memory(
                identity(),
                None,
                HostPolicy::local(RunLimits::default()),
            ),
        )
        .await
        .unwrap();
        let outcome = harness
            .run(RunId::new("run-stream"), "hello")
            .await
            .unwrap();
        assert_eq!(outcome.answer, "streamed answer");
        let streamed = outcome
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                ternilo_protocol::SessionEventKind::AssistantMessageDelta { delta, .. } => {
                    Some(delta.as_str())
                }
                _ => None,
            })
            .collect::<String>();
        assert_eq!(streamed, outcome.answer);
        let reasoning = outcome
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                ternilo_protocol::SessionEventKind::AssistantReasoningDelta { delta, .. } => {
                    Some(delta.as_str())
                }
                _ => None,
            })
            .collect::<String>();
        assert_eq!(reasoning, "consider this");
        let response = outcome
            .events
            .iter()
            .find_map(|event| match &event.kind {
                ternilo_protocol::SessionEventKind::AssistantMessage { response, .. } => {
                    Some(response)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(response.reasoning_content.as_deref(), Some("consider this"));
        assert_eq!(response.usage.unwrap().reasoning_tokens, 3);
        assert!(
            outcome
                .events
                .windows(2)
                .all(|events| events[1].seq == events[0].seq + 1)
        );
        let request = request.await.unwrap();
        assert!(request.contains("\"stream\":true"));
        harness.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn openai_compatible_provider_persists_retry_lifecycle_events() {
        let (base_url, server) = mock_retry_server().await;
        let mut profile = local_profile();
        let model = profile
            .plugins
            .iter_mut()
            .find(|entry| entry.id == "model")
            .unwrap();
        model.kind = model::KIND.to_owned();
        model.config = json!({
            "provider": "mock-provider",
            "base_url": base_url,
            "model": "mock-retry-model",
            "timeout_ms": 5000,
            "max_attempts": 2,
            "retry_base_delay_ms": 1
        });
        let harness = HarnessSession::boot(
            &catalog().unwrap(),
            &profile,
            ternilo_kernel::HostEnvironment::memory(
                identity(),
                None,
                HostPolicy::local(RunLimits::default()),
            ),
        )
        .await
        .unwrap();
        let outcome = harness.run(RunId::new("run-retry"), "hello").await.unwrap();
        assert_eq!(outcome.answer, "after retry");
        let retry_events = outcome
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                ternilo_protocol::SessionEventKind::ModelRetryScheduled {
                    retry_id,
                    retry,
                    max_retries,
                    delay_ms,
                    failure,
                } => Some((
                    "scheduled",
                    retry_id.clone(),
                    *retry,
                    *max_retries,
                    *delay_ms,
                    Some(failure.clone()),
                )),
                ternilo_protocol::SessionEventKind::ModelRetryStarted { retry_id, retry } => {
                    Some(("started", retry_id.clone(), *retry, 0, 0, None))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(retry_events.len(), 2);
        assert_eq!(retry_events[0].0, "scheduled");
        assert_eq!(retry_events[0].1, "run-retry:1:1");
        assert_eq!(retry_events[0].2, 1);
        assert_eq!(retry_events[0].3, 1);
        assert_eq!(retry_events[0].4, 1);
        assert_eq!(
            retry_events[0].5.as_ref().unwrap().code.as_deref(),
            Some("http_503")
        );
        assert_eq!(retry_events[1].0, "started");
        assert_eq!(retry_events[1].1, retry_events[0].1);
        server.await.unwrap();
        harness.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn openai_compatible_provider_projects_image_attachments() {
        let (base_url, request) = mock_completion_server().await;
        let mut profile = local_profile();
        let model = profile
            .plugins
            .iter_mut()
            .find(|entry| entry.id == "model")
            .unwrap();
        model.kind = model::KIND.to_owned();
        model.config = json!({
            "provider": "mock-provider",
            "base_url": base_url,
            "model": "mock-vision-model",
            "timeout_ms": 5000
        });
        let harness = HarnessSession::boot(
            &catalog().unwrap(),
            &profile,
            ternilo_kernel::HostEnvironment::memory(
                identity(),
                None,
                HostPolicy::local(RunLimits::default()),
            ),
        )
        .await
        .unwrap();
        harness
            .run_with_attachments(
                RunId::new("run-image"),
                "describe this image",
                vec![ternilo_protocol::Attachment {
                    name: "pixel.png".to_owned(),
                    media_type: "image/png".to_owned(),
                    content: "data:image/png;base64,iVBORw0KGgo=".to_owned(),
                }],
            )
            .await
            .unwrap();
        let request = request.await.unwrap();
        assert!(request.contains("image_url"));
        assert!(request.contains("data:image/png;base64,iVBORw0KGgo="));
        harness.shutdown().await.unwrap();
    }
}
