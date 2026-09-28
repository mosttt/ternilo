use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use linorun_core::{Activation, ComponentContext, ComponentDescriptor};
use serde_json::{Value, json};
use ternilo_kernel::{
    ActivityBranch, CommandRegistration, CommandResolver, Commands, CommandsClient,
    DeferredToolSource, HarnessPlugin, HarnessSession, HostEnvironment, HostPolicy, PluginFactory,
    PluginManifest, RunCancellation, ToolEffect, ToolExecutionContext, ToolHandler,
    ToolRegistration, Tools, ToolsClient,
};
use ternilo_protocol::{
    AgentId, CommandDescriptor, CommandInputDescriptor, HarnessError, PluginEntry, RunId,
    RunLimits, SessionEventKind, SessionId, SessionIdentity, SessionServiceKind,
    SessionServiceSnapshot, SessionServiceStatus, TenantId, ToolCall, ToolOutput, ToolSpec, UserId,
};
use tokio::sync::Notify;

type CapturedClients = Arc<Mutex<Option<(ToolsClient, CommandsClient)>>>;

struct CaptureTools(CapturedClients);

linorun_macros::component_descriptor! {
    static CAPTURE: () {
        id: "ternilo/test-tool-source-capture@1",
        requires: [Tools, Commands],
        provides: [],
    }
}

impl HarnessPlugin for CaptureTools {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CAPTURE
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context.context().service::<Tools>().unwrap();
        let commands = context.context().service::<Commands>().unwrap();
        let captured = Arc::clone(&self.0);
        Activation::once(async move {
            *captured.lock().unwrap() = Some((tools, commands));
            Ok(None)
        })
    }
}

async fn harness() -> (HarnessSession, ToolsClient, CommandsClient) {
    let clients = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&clients);
    let mut catalog = crate::catalog().unwrap();
    catalog
        .register(PluginFactory::new(
            PluginManifest {
                kind: "test.capture-tools",
                requires: &["ternilo/tools@1", "ternilo/commands@1"],
                provides: &[],
            },
            move |_| Ok(Arc::new(CaptureTools(Arc::clone(&captured)))),
        ))
        .unwrap();
    let mut profile = crate::local_profile();
    profile.plugins.push(PluginEntry {
        id: "capture".to_owned(),
        kind: "test.capture-tools".to_owned(),
        enabled: true,
        config: json!({}),
    });
    let environment = HostEnvironment::memory(
        SessionIdentity {
            tenant_id: TenantId::new("source-test"),
            user_id: UserId::new("source-test"),
            agent_id: AgentId::new("source-test"),
            session_id: SessionId::new("source-test"),
        },
        None,
        HostPolicy::local(RunLimits::default()),
    );
    let harness = HarnessSession::boot(&catalog, &profile, environment)
        .await
        .unwrap();
    let (tools, commands) = clients.lock().unwrap().take().unwrap();
    (harness, tools, commands)
}

struct RecordingTool {
    answer: String,
    calls: Arc<Mutex<Vec<Value>>>,
}

impl ToolHandler for RecordingTool {
    fn execute<'a>(
        &'a self,
        _: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(arguments);
            Ok(ToolOutput {
                content: self.answer.clone(),
                is_error: false,
            })
        })
    }
}

fn registration(name: &str, answer: &str, calls: Arc<Mutex<Vec<Value>>>) -> ToolRegistration {
    ToolRegistration {
        spec: ToolSpec {
            name: name.to_owned(),
            description: "Fixture tool".to_owned(),
            input_schema: json!({ "type": "object", "properties": { "count": { "type": "integer" } }, "required": ["count"] }),
        },
        effect: ToolEffect::ReadOnly,
        handler: Arc::new(RecordingTool {
            answer: answer.to_owned(),
            calls,
        }),
    }
}

struct TestSource {
    id: String,
    initial: Vec<ToolRegistration>,
    tools: Mutex<Vec<ToolRegistration>>,
    blocked: AtomicBool,
    stopped: AtomicBool,
    prepares: AtomicUsize,
    stops: AtomicUsize,
    shutdowns: AtomicUsize,
    entered: Notify,
    released: Notify,
}

impl TestSource {
    fn new(id: &str, tools: Vec<ToolRegistration>) -> Self {
        Self {
            id: id.to_owned(),
            initial: Vec::new(),
            tools: Mutex::new(tools),
            blocked: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            prepares: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            shutdowns: AtomicUsize::new(0),
            entered: Notify::new(),
            released: Notify::new(),
        }
    }
}

impl DeferredToolSource for TestSource {
    fn snapshot(&self) -> SessionServiceSnapshot {
        SessionServiceSnapshot {
            id: self.id.clone(),
            name: self.id.clone(),
            kind: SessionServiceKind::Mcp,
            status: if self.shutdowns.load(Ordering::Acquire) > 0
                || self.stopped.load(Ordering::Acquire)
            {
                SessionServiceStatus::Stopped
            } else if self.prepares.load(Ordering::Acquire) > 0 {
                SessionServiceStatus::Running
            } else {
                SessionServiceStatus::Idle
            },
            active_calls: 0,
            error: None,
        }
    }

    fn initial_tools(&self) -> Vec<ToolRegistration> {
        self.initial.clone()
    }

    fn prepare<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolRegistration>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            cancellation.check()?;
            if self.shutdowns.load(Ordering::Acquire) > 0 {
                return Err(HarnessError::cancelled("source was permanently unmounted"));
            }
            if self.stopped.load(Ordering::Acquire) {
                return Ok(Vec::new());
            }
            self.prepares.fetch_add(1, Ordering::AcqRel);
            self.entered.notify_one();
            if self.blocked.load(Ordering::Acquire) {
                self.released.notified().await;
            }
            // Deliberately return a discovery that was already in flight when unmounted.
            Ok(self.tools.lock().unwrap().clone())
        })
    }

    fn start<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolRegistration>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if self.shutdowns.load(Ordering::Acquire) > 0 {
                return Err(HarnessError::cancelled("source was permanently unmounted"));
            }
            self.stopped.store(false, Ordering::Release);
            self.prepare(cancellation).await
        })
    }

    fn stop<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.stopped.store(true, Ordering::Release);
            self.stops.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    }

    fn shutdown<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.shutdowns.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    }
}

async fn invoke(tools: &ToolsClient, name: &str) -> ToolOutput {
    tools
        .execute(
            RunId::new("registry-test"),
            ToolCall {
                id: "registry-call".to_owned(),
                name: name.to_owned(),
                arguments: json!({"count": 1}),
                presentation: None,
            },
            RunCancellation::new(),
            ActivityBranch::untracked(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn conflicting_initial_or_discovered_source_tools_leave_native_tools_intact() {
    let (harness, tools, _) = harness().await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    tools
        .register_tool(registration(
            "native_fixture",
            "native-result",
            Arc::clone(&calls),
        ))
        .await
        .unwrap();
    let before = tools.list().await;
    let mut initial = TestSource::new("bad-initial", Vec::new());
    initial.initial = vec![
        registration("partial_initial", "incorrect", Arc::clone(&calls)),
        registration("native_fixture", "incorrect", Arc::clone(&calls)),
    ];
    assert!(tools.register_source(Arc::new(initial)).await.is_err());
    assert_eq!(tools.list().await, before);
    assert!(tools.sources().await.is_empty());
    let source = Arc::new(TestSource::new(
        "bad-discovery",
        vec![
            registration("partial_discovered", "incorrect", Arc::clone(&calls)),
            registration("native_fixture", "incorrect", Arc::clone(&calls)),
        ],
    ));
    let id = tools.register_source(source.clone()).await.unwrap();
    assert!(tools.prepare(RunCancellation::new()).await.is_err());
    assert_eq!(tools.list().await, before);
    assert_eq!(
        invoke(&tools, "native_fixture").await.content,
        "native-result"
    );
    assert_eq!(source.stops.load(Ordering::Acquire), 1);
    assert_eq!(source.shutdowns.load(Ordering::Acquire), 0);
    assert_eq!(source.snapshot().status, SessionServiceStatus::Stopped);
    assert!(
        tools
            .start_source("bad-discovery".to_owned(), RunCancellation::new())
            .await
            .is_err()
    );
    assert_eq!(source.stops.load(Ordering::Acquire), 2);
    assert_eq!(source.shutdowns.load(Ordering::Acquire), 0);
    assert_eq!(tools.list().await, before);
    *source.tools.lock().unwrap() = vec![registration(
        "corrected_tool",
        "recovered",
        Arc::clone(&calls),
    )];
    let recovered = tools
        .start_source("bad-discovery".to_owned(), RunCancellation::new())
        .await
        .unwrap();
    assert_eq!(recovered.status, SessionServiceStatus::Running);
    assert_eq!(invoke(&tools, "corrected_tool").await.content, "recovered");
    assert_eq!(
        invoke(&tools, "native_fixture").await.content,
        "native-result"
    );
    tools.unregister_source(id).await.unwrap();
    assert_eq!(tools.list().await, before);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn source_replacement_publishes_as_one_catalog_and_conflict_keeps_previous_catalog() {
    let (harness, tools, _) = harness().await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    tools
        .register_tool(registration(
            "native_fixture",
            "native-result",
            Arc::clone(&calls),
        ))
        .await
        .unwrap();
    let source = Arc::new(TestSource::new(
        "replaceable",
        vec![registration("old_source_tool", "old", Arc::clone(&calls))],
    ));
    let id = tools.register_source(source.clone()).await.unwrap();
    tools.prepare(RunCancellation::new()).await.unwrap();
    source.entered.notified().await;
    let before = tools.list().await;
    *source.tools.lock().unwrap() = vec![
        registration("new_one", "new-one", Arc::clone(&calls)),
        registration("new_two", "new-two", Arc::clone(&calls)),
    ];
    source.blocked.store(true, Ordering::Release);
    let client = tools.clone();
    let preparation = tokio::spawn(async move { client.prepare(RunCancellation::new()).await });
    source.entered.notified().await;
    assert_eq!(tools.list().await, before);
    source.released.notify_one();
    preparation.await.unwrap().unwrap();
    let published = tools.list().await;
    assert!(!published.iter().any(|tool| tool.name == "old_source_tool"));
    assert!(published.iter().any(|tool| tool.name == "new_one"));
    assert!(published.iter().any(|tool| tool.name == "new_two"));
    source.blocked.store(false, Ordering::Release);
    *source.tools.lock().unwrap() = vec![
        registration("partial_replacement", "incorrect", Arc::clone(&calls)),
        registration("native_fixture", "incorrect", Arc::clone(&calls)),
    ];
    assert!(tools.prepare(RunCancellation::new()).await.is_err());
    assert_eq!(tools.list().await, published);
    assert_eq!(source.stops.load(Ordering::Acquire), 1);
    assert_eq!(source.shutdowns.load(Ordering::Acquire), 0);
    assert_eq!(
        invoke(&tools, "native_fixture").await.content,
        "native-result"
    );
    tools.unregister_source(id).await.unwrap();
    let retained = tools.list().await;
    assert!(retained.iter().any(|tool| tool.name == "native_fixture"));
    assert!(
        !retained
            .iter()
            .any(|tool| matches!(tool.name.as_str(), "new_one" | "new_two"))
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn unmount_during_preparation_cannot_publish_over_a_new_source_with_the_same_name() {
    let (harness, tools, _) = harness().await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let source = Arc::new(TestSource::new(
        "reused-name",
        vec![registration("retired_tool", "retired", Arc::clone(&calls))],
    ));
    source.blocked.store(true, Ordering::Release);
    let id = tools.register_source(source.clone()).await.unwrap();
    let client = tools.clone();
    let preparation = tokio::spawn(async move { client.prepare(RunCancellation::new()).await });
    source.entered.notified().await;
    let client = tools.clone();
    let removal = tokio::spawn(async move { client.unregister_source(id).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while source.shutdowns.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(tools.sources().await.is_empty());
    let replacement = Arc::new(TestSource::new(
        "reused-name",
        vec![registration("replacement_tool", "replacement", calls)],
    ));
    let replacement_id = tools.register_source(replacement.clone()).await.unwrap();
    tools.prepare(RunCancellation::new()).await.unwrap();
    source.released.notify_one();
    assert!(preparation.await.unwrap().unwrap_err().is_cancelled());
    removal.await.unwrap().unwrap();
    assert!(source.shutdowns.load(Ordering::Acquire) > 0);
    assert_eq!(source.stops.load(Ordering::Acquire), 0);
    assert!(source.start(RunCancellation::new()).await.is_err());
    let catalog = tools.list().await;
    assert!(!catalog.iter().any(|tool| tool.name == "retired_tool"));
    assert_eq!(
        invoke(&tools, "replacement_tool").await.content,
        "replacement"
    );
    assert_eq!(replacement.shutdowns.load(Ordering::Acquire), 0);
    tools.unregister_source(replacement_id).await.unwrap();
    harness.shutdown().await.unwrap();
}

struct CountResolver;

impl CommandResolver for CountResolver {
    fn resolve(&self, input: &str) -> Result<Value, HarnessError> {
        serde_json::from_str::<Value>(input)
            .map(|count| json!({"count": count}))
            .map_err(|error| HarnessError::invalid(error.to_string()))
    }
}

#[tokio::test]
async fn direct_command_discovers_its_cold_source_before_resolving_and_validating_arguments() {
    let (harness, tools, commands) = harness().await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let source = Arc::new(TestSource::new(
        "cold",
        vec![registration("cold_tool", "cold-result", Arc::clone(&calls))],
    ));
    let id = tools.register_source(source.clone()).await.unwrap();
    let command = commands
        .register_command(CommandRegistration {
            descriptor: CommandDescriptor {
                name: "cold".to_owned(),
                description: "Call the discovered tool".to_owned(),
                input: Some(CommandInputDescriptor {
                    hint: "<count>".to_owned(),
                    images: false,
                }),
            },
            tool_name: "cold_tool".to_owned(),
            resolver: Arc::new(CountResolver),
        })
        .await
        .unwrap();
    assert!(
        commands
            .resolve("/cold 42".to_owned())
            .await
            .unwrap()
            .result
            .is_err()
    );
    assert_eq!(source.prepares.load(Ordering::Acquire), 0);
    let outcome = harness
        .run(RunId::new("cold-valid"), "/cold 42")
        .await
        .unwrap();
    assert_eq!(outcome.answer, "cold-result");
    assert_eq!(*calls.lock().unwrap(), vec![json!({"count": 42})]);
    assert!(outcome.events.iter().any(|event| matches!(&event.kind, SessionEventKind::ToolCallStarted { call } if call.name == "cold_tool")));
    assert!(
        !outcome
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::ModelRequestStarted { .. }))
    );
    let invalid = harness
        .run(RunId::new("cold-invalid"), "/cold \"wrong\"")
        .await
        .unwrap();
    assert!(invalid.answer.contains("target tool input schema"));
    assert_eq!(invalid.tool_calls, 0);
    assert_eq!(calls.lock().unwrap().len(), 1);
    commands.unregister_command(command).await.unwrap();
    tools.unregister_source(id).await.unwrap();
    harness.shutdown().await.unwrap();
}
