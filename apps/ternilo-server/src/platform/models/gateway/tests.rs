use std::{sync::Arc, time::Duration};

use futures_util::StreamExt as _;
use salvo_core::{conn::tcp::TcpAcceptor, server::ServerHandle};
use salvo_extra::affix_state;
use ternilo_cloud::{CloudSessionEventFeed, CloudStore};
use ternilo_control::{
    ControlStore, InstanceMode, ModelGrantInput, ModelGrantSubject, ModelKeyCreation,
    ModelKeyInput, ModelProviderInput, ModelPublicationInput, NativeRegistration,
    NativeSessionGrant, SecretCipher,
};
use ternilo_protocol::{
    ProviderModel, ProviderModelDefaults, ProviderModelReasoning, ProviderModelSettings,
    ProviderProfile, ReasoningEffort,
};
use tokio::sync::Mutex;

use super::*;
use crate::platform::edge::EdgeGateway;
mod native;

fn defaults() -> ProviderModelDefaults {
    ProviderModelDefaults {
        context_window: 32_000,
        max_output_tokens: 512,
        reasoning: None,
    }
}

#[test]
fn native_request_keeps_tools_images_reasoning_and_canonical_identity() {
    let body = json!({
        "model":"public", "stream":true, "n":2, "max_completion_tokens":100,
        "messages":[{"role":"user","content":[{"type":"text","text":"Describe"},{"type":"image_url","image_url":{"url":"https://example.test/image.png","detail":"low"}}]}],
        "tools":[{"type":"function","function":{"name":"lookup","strict":true,"parameters":{"type":"object","properties":{"file_id":{"type":"string"}},"additionalProperties":false}}}],
        "tool_choice":{"type":"function","function":{"name":"lookup"}},
        "response_format":{"type":"json_schema","json_schema":{"name":"result","strict":true,"schema":{"type":"object"}}},
        "reasoning_effort":"high", "stream_options":{"include_usage":false,"include_obfuscation":false},
    });
    let mut prepared =
        PreparedRequest::parse(body.clone(), ProviderProtocol::OpenAiChatCompletions).unwrap();
    let hash = prepared.payload_hash.clone();
    let mut model_defaults = defaults();
    model_defaults.reasoning = Some(ProviderModelReasoning {
        default_effort: ReasoningEffort::High,
        efforts: [(ReasoningEffort::High, Some("high".to_owned()))]
            .into_iter()
            .collect(),
    });
    assert_eq!(
        prepared
            .prepare_upstream(
                ProviderProtocol::OpenAiChatCompletions,
                "internal-alias",
                &model_defaults
            )
            .unwrap(),
        32_200
    );
    assert_eq!(prepared.body["tools"], body["tools"]);
    assert_eq!(prepared.body["messages"], body["messages"]);
    assert_eq!(prepared.body["response_format"], body["response_format"]);
    assert_eq!(prepared.body["stream_options"]["include_usage"], true);
    assert_eq!(
        prepared.body["stream_options"]["include_obfuscation"],
        false
    );
    assert_eq!(prepared.body["model"], "internal-alias");
    assert_eq!(prepared.payload_hash, hash);
    let equivalent: Value = serde_json::from_str(&body.to_string()).unwrap();
    assert_eq!(
        PreparedRequest::parse(equivalent, ProviderProtocol::OpenAiChatCompletions)
            .unwrap()
            .payload_hash,
        hash
    );
    let first = PreparedRequest::parse(
        serde_json::from_str(r#"{"model":"public","messages":[{"role":"user","content":"test"}]}"#)
            .unwrap(),
        ProviderProtocol::OpenAiChatCompletions,
    )
    .unwrap();
    let reordered = PreparedRequest::parse(
        serde_json::from_str(r#"{"messages":[{"content":"test","role":"user"}],"model":"public"}"#)
            .unwrap(),
        ProviderProtocol::OpenAiChatCompletions,
    )
    .unwrap();
    assert_eq!(first.payload_hash, reordered.payload_hash);
}

#[test]
fn published_reasoning_constraints_apply_native_values_and_defaults() {
    let mut settings = defaults();
    settings.reasoning = Some(ProviderModelReasoning {
        default_effort: ReasoningEffort::High,
        efforts: [
            (ReasoningEffort::High, Some("ultra".to_owned())),
            (ReasoningEffort::Low, None),
        ]
        .into_iter()
        .collect(),
    });
    let mut prepared = PreparedRequest::parse(
        json!({"model":"public","input":"hello","reasoning":{"summary":"auto"}}),
        ProviderProtocol::OpenAiResponses,
    )
    .unwrap();
    prepared
        .prepare_upstream(ProviderProtocol::OpenAiResponses, "upstream", &settings)
        .unwrap();
    assert_eq!(
        prepared.body["reasoning"],
        json!({"effort":"ultra","summary":"auto"})
    );
    for effort in ["high", "max", "low"] {
        let mut prepared = PreparedRequest::parse(
            json!({"model":"public","input":"hello","reasoning":{"effort":effort}}),
            ProviderProtocol::OpenAiResponses,
        )
        .unwrap();
        assert!(
            prepared
                .prepare_upstream(ProviderProtocol::OpenAiResponses, "upstream", &settings)
                .is_err()
        );
    }
    let body = json!({"model":"public","messages":[{"role":"user","content":"hello"}],"reasoning_effort":"ultra"});
    let mut prepared =
        PreparedRequest::parse(body.clone(), ProviderProtocol::OpenAiChatCompletions).unwrap();
    prepared
        .prepare_upstream(
            ProviderProtocol::OpenAiChatCompletions,
            "upstream",
            &settings,
        )
        .unwrap();
    assert_eq!(prepared.body["reasoning_effort"], "ultra");
    let mut unsupported =
        PreparedRequest::parse(body, ProviderProtocol::OpenAiChatCompletions).unwrap();
    assert!(
        unsupported
            .prepare_upstream(
                ProviderProtocol::OpenAiChatCompletions,
                "upstream",
                &defaults()
            )
            .is_err()
    );
    settings.reasoning.as_mut().unwrap().default_effort = ReasoningEffort::Low;
    let mut prepared = PreparedRequest::parse(
        json!({"model":"public","input":"hello","reasoning":{"summary":"auto"}}),
        ProviderProtocol::OpenAiResponses,
    )
    .unwrap();
    prepared
        .prepare_upstream(ProviderProtocol::OpenAiResponses, "upstream", &settings)
        .unwrap();
    assert_eq!(prepared.body["reasoning"], json!({"summary":"auto"}));
}

#[test]
fn stateless_responses_rejects_upstream_objects_and_hosted_tools() {
    let cases = [
        json!({"model":"m","input":"x","previous_response_id":"resp-other-user"}),
        json!({"model":"m","input":"x","conversation":"conv-other-user"}),
        json!({"model":"m","input":"x","store":true}),
        json!({"model":"m","input":"x","background":true}),
        json!({"model":"m","input":[{"type":"item_reference","id":"msg-other-user"}]}),
        json!({"model":"m","input":[{"role":"user","content":[{"type":"input_image","file_id":"file-other-user"}]}]}),
        json!({"model":"m","input":"x","tools":[{"type":"web_search"}]}),
        json!({"model":"m","input":"x","tool_choice":{"type":"web_search"}}),
    ];
    for body in cases {
        assert!(PreparedRequest::parse(body, ProviderProtocol::OpenAiResponses).is_err());
    }
    let body = json!({"model":"m","input":[
        {"type":"reasoning","id":"rs-1","encrypted_content":"opaque-owned-history","summary":[]},
        {"type":"function_call","call_id":"call-1","name":"lookup","arguments":"{}"},
        {"type":"function_call_output","call_id":"call-1","output":"done"}
    ],"text":{"format":{"type":"json_object"}},"reasoning":{"effort":"high","summary":"auto"},"include":["reasoning.encrypted_content"]});
    assert_eq!(
        PreparedRequest::parse(body.clone(), ProviderProtocol::OpenAiResponses)
            .unwrap()
            .body,
        body
    );
}

#[test]
fn usage_preserves_unknown_details_and_does_not_add_reasoning_twice() {
    let value = json!({"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_cache_hit_tokens":70,"cache_creation_input_tokens":8,"completion_tokens_details":{"reasoning_tokens":12}}});
    let usage = usage::parse_usage(&value, ProviderProtocol::OpenAiChatCompletions).unwrap();
    assert_eq!(usage.input_tokens, Some(100));
    assert_eq!(usage.output_tokens, Some(20));
    assert_eq!(usage.cached_input_tokens, Some(70));
    assert_eq!(usage.cache_write_tokens, Some(8));
    assert_eq!(usage.reasoning_tokens, Some(12));
    assert_eq!(usage.raw_usage, Some(value["usage"].clone()));
    let partial = usage::parse_usage(
        &json!({"usage":{"input_tokens":7}}),
        ProviderProtocol::OpenAiResponses,
    )
    .unwrap();
    assert_eq!(partial.input_tokens, Some(7));
    assert_eq!(partial.output_tokens, None);
    assert_eq!(partial.cached_input_tokens, None);
    assert_eq!(partial.reasoning_tokens, None);
}

struct RunningServer {
    base: String,
    handle: ServerHandle,
    task: tokio::task::JoinHandle<()>,
}

impl RunningServer {
    async fn start(router: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
        let handle = server.handle();
        let task = tokio::spawn(async move {
            server.try_serve(router).await.unwrap();
        });
        Self {
            base: format!("http://{address}"),
            handle,
            task,
        }
    }

    async fn close(self) {
        self.handle.stop_graceful(Some(Duration::from_secs(1)));
        self.task.await.unwrap();
    }
}

#[derive(Clone, Default)]
struct UpstreamState {
    requests: Arc<Mutex<Vec<Value>>>,
}

#[handler]
async fn fake_upstream(request: &mut Request, depot: &mut Depot, response: &mut Response) {
    assert_eq!(
        request.headers().get(header::AUTHORIZATION).unwrap(),
        "Bearer upstream-test-secret"
    );
    let body = request.parse_json::<Value>().await.unwrap();
    depot
        .get_typed::<UpstreamState>()
        .unwrap()
        .requests
        .lock()
        .await
        .push(body.clone());
    let responses = request.uri().path().ends_with("responses");
    let case = body["metadata"]["test_case"].as_str().unwrap_or("normal");
    if case == "reject" {
        response.status_code(StatusCode::SERVICE_UNAVAILABLE);
        response.render(Json(
            json!({"error":{"message":"upstream-test-secret at internal-route"}}),
        ));
        return;
    }
    response
        .headers_mut()
        .insert("x-request-id", "upstream-request-1".parse().unwrap());
    let usage = if responses {
        json!({"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":70},"output_tokens_details":{"reasoning_tokens":12},"cache_creation_input_tokens":8})
    } else {
        json!({"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":70},"completion_tokens_details":{"reasoning_tokens":12},"cache_creation_input_tokens":8})
    };
    let function = json!({"type":"function_call","id":"fc-upstream","call_id":"call-upstream","name":"lookup","arguments":"{\"path\":\"file.txt\"}","status":"completed"});
    let deepseek = body["metadata"]["test_protocol"] == "deepseek";
    let reasoning = if deepseek {
        json!({"type":"reasoning","id":"rs-1","summary":[],"content":[{"type":"reasoning_text","text":"Full reasoning"}]})
    } else {
        json!({"type":"reasoning","id":"rs-1","summary":[],"encrypted_content":"opaque-reasoning"})
    };
    let final_value = if responses {
        json!({"id":"resp-upstream","object":"response","created_at":1,"model":"internal-model","status":"completed","output":[reasoning,function.clone()],"usage":usage})
    } else {
        json!({"id":"chat-upstream","object":"chat.completion","created":1,"model":"internal-model","choices":[{"index":0,"message":{"role":"assistant","content":null,"reasoning_content":"Thinking","tool_calls":[{"id":"call-upstream","type":"function","function":{"name":"lookup","arguments":"{\"path\":\"file.txt\"}"}}]},"finish_reason":"tool_calls","logprobs":null}],"usage":usage})
    };
    if !body["stream"].as_bool().unwrap_or(false) {
        response.render(Json(final_value));
        return;
    }
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
    let protocol = if responses {
        ProviderProtocol::OpenAiResponses
    } else {
        ProviderProtocol::OpenAiChatCompletions
    };
    let mut events = if responses {
        vec![
            json!({"type":"response.created","sequence_number":0,"response":{"id":"resp-upstream","model":"internal-model","status":"in_progress","output":[]}}),
            json!({"type":"response.output_item.added","sequence_number":1,"output_index":1,"item":{"type":"function_call","id":"fc-upstream","call_id":"call-upstream","name":"lookup","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","sequence_number":2,"output_index":1,"item_id":"fc-upstream","delta":"{\"path\":\"file.txt\"}"}),
            json!({"type":"response.output_item.done","sequence_number":3,"output_index":1,"item":function}),
        ]
    } else {
        vec![
            json!({"id":"chat-upstream","object":"chat.completion.chunk","model":"internal-model","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"Thinking","tool_calls":[{"index":0,"id":"call-upstream","type":"function","function":{"name":"lookup","arguments":"{\"path\":\"file.txt\"}"}}]},"finish_reason":null}]}),
            json!({"id":"chat-upstream","object":"chat.completion.chunk","model":"internal-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ]
    };
    if deepseek {
        events.push(json!({"type":"response.reasoning_text.delta","output_index":0,"content_index":0,"delta":"Full reasoning"}));
    }
    if case != "broken" && case != "stall" {
        if responses {
            let mut final_value = final_value;
            if case == "missing_usage" {
                final_value.as_object_mut().unwrap().remove("usage");
            }
            events.push(
                json!({"type":"response.completed","sequence_number":4,"response":final_value}),
            );
        } else if case != "missing_usage" {
            events.push(json!({"id":"chat-upstream","object":"chat.completion.chunk","model":"internal-model","choices":[],"usage":usage}));
        }
    }
    let mut bytes: Vec<_> = events
        .iter()
        .flat_map(|event| protocol::sse_frame(event, protocol))
        .collect();
    if !responses && case != "broken" && case != "stall" {
        bytes.extend_from_slice(b"data: [DONE]\n\n");
    }
    let chunks: Vec<_> = bytes.chunks(17).map(<[u8]>::to_vec).collect();
    let stall = case == "stall";
    response.stream(
        futures_util::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>)).chain(
            futures_util::stream::once(async move {
                if stall {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                Ok(Vec::<u8>::new())
            }),
        ),
    );
}

struct Fixture {
    state: AppState,
    owner: NativeSessionGrant,
    key: ModelKeyCreation,
    server: RunningServer,
    upstream: RunningServer,
    captured: UpstreamState,
    shutdown: tokio::sync::watch::Sender<bool>,
    client: reqwest::Client,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_native(false).await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Create isolated real HTTP servers and a complete model grant without managed execution."
    )]
    async fn with_native(native: bool) -> Self {
        let now = now_ms().unwrap();
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([73; 32]), 1)
                .await
                .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "owner@example.test".to_owned(),
                    username: "owner".to_owned(),
                    password: "owner-model-test-password".to_owned(),
                },
                now,
            )
            .await
            .unwrap();
        store
            .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, now)
            .await
            .unwrap();
        let captured = UpstreamState::default();
        let upstream = RunningServer::start(
            Router::new()
                .hoop(affix_state::inject(captured.clone()))
                .push(Router::with_path("v1/chat/completions").post(fake_upstream))
                .push(Router::with_path("v1/responses").post(fake_upstream))
                .push(Router::with_path("v1/messages").post(native::fake_upstream))
                .push(Router::with_path("v1/models/{model_action}").post(native::fake_upstream)),
        )
        .await;
        let mut protocols = vec![
            ("chat", ProviderProtocol::OpenAiChatCompletions),
            ("responses", ProviderProtocol::OpenAiResponses),
            ("deepseek", ProviderProtocol::DeepSeekResponses),
        ];
        if native {
            protocols.extend([
                ("gemini", ProviderProtocol::GoogleGemini),
                ("claude", ProviderProtocol::AnthropicMessages),
            ]);
        }
        let model_ids = protocols
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect::<Vec<_>>();
        for (name, protocol) in protocols {
            store
                .save_model_provider(
                    &owner.session.user,
                    &ModelProviderInput {
                        profile: ProviderProfile {
                            id: name.to_owned(),
                            display_name: name.to_owned(),
                            base_url: format!("{}/v1", upstream.base),
                            protocol,
                            api_key_ref: None,
                            defaults: defaults(),
                            models: vec![ProviderModel {
                                id: "internal-model".to_owned(),
                                display_name: None,
                                settings: ProviderModelSettings::Inherit,
                            }],
                            timeout_ms: 20_000,
                            max_attempts: 3,
                            retry_base_delay_ms: 250,
                        },
                        enabled: true,
                        api_key: Some("upstream-test-secret".to_owned()),
                        clear_api_key: false,
                    },
                    now,
                )
                .await
                .unwrap();
            store
                .save_model_publication(
                    &owner.session.user,
                    &ModelPublicationInput {
                        model_id: name.to_owned(),
                        display_name: format!("Public {name}"),
                        provider_id: name.to_owned(),
                        upstream_model: "internal-model".to_owned(),
                        enabled: true,
                    },
                    now,
                )
                .await
                .unwrap();
        }
        let grant = store
            .save_model_grant(
                &owner.session.user,
                None,
                &ModelGrantInput {
                    name: "Personal allowance".to_owned(),
                    subject: ModelGrantSubject::User {
                        id: owner.session.user.user_id.as_str().to_owned(),
                    },
                    model_ids: model_ids.clone(),
                    monthly_tokens: 1_000_000,
                    max_concurrent_requests: 2,
                    allow_resource_sharing: true,
                    expires_at_ms: None,
                },
                now,
            )
            .await
            .unwrap();
        let key = store
            .create_model_key(
                &owner.session.user,
                &ModelKeyInput {
                    name: "Test client".to_owned(),
                    grant_id: grant.grant_id,
                    model_ids,
                    monthly_tokens: None,
                    max_concurrent_requests: None,
                    expires_at_ms: None,
                },
                now,
            )
            .await
            .unwrap();
        let cloud = CloudStore::from_database(store.database().clone())
            .await
            .unwrap();
        let catalog = ternilo_cloud::catalog().unwrap();
        let policy = crate::platform::load_worker_policy(None, &catalog).unwrap();
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let state = AppState {
            cloud_events: CloudSessionEventFeed::from_database(store.database().clone())
                .await
                .unwrap(),
            edge: Arc::new(EdgeGateway::new(store.edge_store()).await.unwrap()),
            store,
            cloud,
            security: Arc::default(),
            setup_token_hash: None,
            managed_execution_enabled: false,
            shutdown: receiver,
            worker_policy: Arc::new(policy),
            catalog: Arc::new(catalog),
        };
        let server = RunningServer::start(crate::platform::web_router(state.clone())).await;
        Self {
            state,
            owner,
            key,
            server,
            upstream,
            captured,
            shutdown,
            client: reqwest::Client::new(),
        }
    }

    async fn request(
        &self,
        model: &str,
        stream: bool,
        case: &str,
        idempotency: &str,
    ) -> reqwest::Response {
        let body = if model == "chat" {
            json!({"model":model,"stream":stream,"messages":[{"role":"user","content":"Call lookup"}],"metadata":{"test_case":case}})
        } else {
            json!({"model":model,"stream":stream,"input":"Call lookup","metadata":{"test_case":case,"test_protocol":model}})
        };
        let path = if model == "chat" {
            "chat/completions"
        } else {
            "responses"
        };
        self.client
            .post(format!("{}/v1/{path}", self.server.base))
            .bearer_auth(&self.key.token)
            .header("idempotency-key", idempotency)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn ledger(&self, request_id: &str) -> Value {
        let page = self
            .state
            .store
            .list_model_service_requests(
                &self.owner.session.user,
                None,
                &ternilo_control::PageQuery {
                    query: Some(request_id.to_owned()),
                    ..ternilo_control::PageQuery::default()
                },
            )
            .await
            .unwrap();
        let request = page
            .requests
            .into_iter()
            .find(|request| request.request_id == request_id)
            .expect("accepted model request remains in the independent ledger");
        serde_json::to_value(request).unwrap()
    }

    async fn close(self) {
        self.shutdown.send_replace(true);
        self.server.close().await;
        self.state.edge.shutdown().await;
        self.upstream.close().await;
    }
}

#[tokio::test]
async fn model_api_preserves_native_protocols_and_meters_without_workers() {
    let fixture = Fixture::new().await;
    let catalog = fixture
        .client
        .get(format!("{}/v1/models", fixture.server.base))
        .bearer_auth(&fixture.key.token)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(catalog["data"].as_array().unwrap().len(), 3);
    assert!(!catalog.to_string().contains("internal-model"));
    assert!(!catalog.to_string().contains("upstream-test-secret"));
    for model in ["chat", "responses", "deepseek"] {
        for stream in [false, true] {
            let response = fixture
                .request(model, stream, "normal", &format!("{model}-{stream}"))
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let request_id = response.headers()["x-ternilo-request-id"]
                .to_str()
                .unwrap()
                .to_owned();
            assert_eq!(response.headers()["x-request-id"], request_id);
            let text = response.text().await.unwrap();
            assert!(!text.contains("internal-model"), "{text}");
            assert!(text.contains("call-upstream"), "{text}");
            assert!(text.contains("file.txt"), "{text}");
            if model == "responses" {
                assert!(text.contains("opaque-reasoning"));
            }
            if stream {
                assert!(text.contains(if model == "chat" {
                    "[DONE]"
                } else {
                    "response.completed"
                }));
            }
            let ledger = fixture.ledger(&request_id).await;
            if model == "deepseek" {
                assert_eq!(ledger["protocol"], "deepseek-responses");
                assert!(text.contains("Full reasoning"));
                if stream {
                    assert!(text.contains("response.reasoning_text.delta"));
                }
            }
            assert_eq!(ledger["state"], "completed");
            assert_eq!(ledger["accounted_tokens"], 120);
            assert_eq!(ledger["usage"]["cache_write_tokens"], 8);
            assert_eq!(ledger["usage"]["reasoning_tokens"], 12);
            let duplicate = fixture
                .request(model, stream, "normal", &format!("{model}-{stream}"))
                .await;
            assert_eq!(duplicate.status(), StatusCode::CONFLICT);
        }
    }
    assert_eq!(fixture.captured.requests.lock().await.len(), 6);
    fixture.close().await;
}

#[tokio::test]
async fn unknown_usage_and_broken_streams_keep_reservations_and_errors_hide_secrets() {
    let fixture = Fixture::new().await;
    for case in ["missing_usage", "broken", "reject"] {
        let response = fixture.request("chat", true, case, case).await;
        let request_id = response.headers()["x-ternilo-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            response.status(),
            if case == "reject" {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::OK
            }
        );
        let text = response.text().await.unwrap();
        assert!(!text.contains("upstream-test-secret"));
        if case == "broken" {
            assert!(!text.contains("[DONE]"));
            assert!(text.contains("upstream_stream_incomplete"));
        }
        let ledger = fixture.ledger(&request_id).await;
        assert_eq!(ledger["accounted_tokens"], Value::Null);
        assert!(ledger["reserved_tokens"].as_u64().unwrap() > 0);
    }
    assert_eq!(fixture.captured.requests.lock().await.len(), 3);
    fixture.close().await;
}

#[tokio::test]
async fn active_model_stream_rechecks_key_revocation_and_settles_after_revocation() {
    let fixture = Fixture::new().await;
    let response = fixture
        .request("chat", true, "stall", "cancel-stream")
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = response.headers()["x-ternilo-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    fixture
        .state
        .store
        .revoke_model_key(
            &fixture.owner.session.user,
            &fixture.key.key.key_id,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let text = tokio::time::timeout(Duration::from_secs(5), response.text())
        .await
        .unwrap()
        .unwrap();
    assert!(
        text.contains("model_access_denied") || text.contains("invalid_api_key"),
        "{text}"
    );
    assert!(!text.contains("[DONE]"));
    let ledger = fixture.ledger(&request_id).await;
    assert_eq!(ledger["state"], "cancelled");
    assert_eq!(ledger["accounted_tokens"], Value::Null);
    let denied = fixture
        .request("chat", false, "normal", "after-revoke")
        .await;
    assert!(matches!(
        denied.status(),
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
    ));
    assert_eq!(fixture.captured.requests.lock().await.len(), 1);
    fixture.close().await;
}

#[tokio::test]
async fn model_endpoint_rejects_account_credentials_protocol_mismatch_and_over_limit() {
    let fixture = Fixture::new().await;
    for token in [
        &fixture.owner.access_token,
        "node-credential",
        "worker-credential",
    ] {
        let response = fixture
            .client
            .get(format!("{}/v1/models", fixture.server.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    for body in [
        json!({"model":"responses","messages":[{"role":"user","content":"hello"}]}),
        json!({"model":"chat","max_tokens":999_999,"messages":[{"role":"user","content":"hello"}]}),
        json!({"model":"chat","n":0,"messages":[{"role":"user","content":"hello"}]}),
    ] {
        let response = fixture
            .client
            .post(format!("{}/v1/chat/completions", fixture.server.base))
            .bearer_auth(&fixture.key.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(fixture.captured.requests.lock().await.is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn disconnected_clients_release_concurrency_without_zeroing_unknown_usage() {
    let fixture = Fixture::new().await;
    let first = fixture.request("chat", true, "stall", "first-active").await;
    let second = fixture
        .request("chat", true, "stall", "second-active")
        .await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    let first_id = first.headers()["x-ternilo-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let second_id = second.headers()["x-ternilo-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let rejected = fixture
        .request("chat", false, "normal", "over-concurrency")
        .await;
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
    drop(first);
    drop(second);
    for request_id in [&first_id, &second_id] {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let ledger = fixture.ledger(request_id).await;
                if ledger["state"] == "cancelled" {
                    assert_eq!(ledger["accounted_tokens"], Value::Null);
                    assert!(ledger["reserved_tokens"].as_u64().unwrap() > 0);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    let next = fixture
        .request("chat", false, "normal", "after-disconnect")
        .await;
    assert_eq!(next.status(), StatusCode::OK);
    assert_eq!(fixture.captured.requests.lock().await.len(), 3);
    fixture.close().await;
}
