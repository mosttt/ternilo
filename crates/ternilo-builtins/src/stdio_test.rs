use std::{process::Stdio, time::Duration};

use tokio::process::Command;

use crate::process_group::OwnedProcessGroup;

linorun_macros::component_descriptor! {
    static ENVIRONMENT_BRIDGE: () {
        id: "ternilo/test-stdio-environment@1",
        requires: [ternilo_kernel::RunEnvironment],
        provides: [],
    }
}

struct EnvironmentBridge {
    sender: std::sync::Mutex<
        Option<tokio::sync::oneshot::Sender<ternilo_kernel::RunEnvironmentClient>>,
    >,
}

impl linorun_core::Component for EnvironmentBridge {
    type Config = ();

    fn descriptor(&self) -> &'static linorun_core::ComponentDescriptor {
        &ENVIRONMENT_BRIDGE
    }

    fn activate(
        &self,
        context: linorun_core::ComponentContext,
        _: std::sync::Arc<Self::Config>,
    ) -> linorun_core::Activation {
        let environment = context
            .context()
            .service::<ternilo_kernel::RunEnvironment>()
            .unwrap();
        let sender = self.sender.lock().unwrap().take().unwrap();
        linorun_core::Activation::Once(Box::pin(async move {
            let _ = sender.send(environment);
            Ok(None)
        }))
    }
}

pub(crate) async fn environment_client(
    environment: ternilo_kernel::HostEnvironment,
) -> (linorun_core::Runtime, ternilo_kernel::RunEnvironmentClient) {
    let runtime = linorun_core::Runtime::builder(ternilo_kernel::TokioSpawner).build();
    let root = runtime.root();
    root.provide::<ternilo_kernel::RunEnvironment>(std::sync::Arc::new(environment))
        .await
        .unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let bridge = root
        .mount(
            EnvironmentBridge {
                sender: std::sync::Mutex::new(Some(sender)),
            },
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        bridge.wait_settled().await,
        linorun_core::FiberState::Active
    );
    (runtime, receiver.await.unwrap())
}

pub(crate) fn tool_context() -> ternilo_kernel::ToolExecutionContext {
    ternilo_kernel::ToolExecutionContext {
        activity: ternilo_kernel::ActivityBranch::untracked(),
        identity: ternilo_protocol::SessionIdentity {
            tenant_id: ternilo_protocol::TenantId::new("stdio-test"),
            user_id: ternilo_protocol::UserId::new("stdio-test"),
            agent_id: ternilo_protocol::AgentId::new("stdio-test"),
            session_id: ternilo_protocol::SessionId::new("stdio-test"),
        },
        workspace: None,
        run_id: ternilo_protocol::RunId::new("stdio-test"),
        call_id: "stdio-test".to_owned(),
        cancellation: ternilo_kernel::RunCancellation::new(),
    }
}

pub(crate) struct StdioFixture {
    directory: tempfile::TempDir,
}

impl StdioFixture {
    pub(crate) fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
        }
    }

    pub(crate) fn command(&self, protocol: &str, mode: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(SERVER)
            .env("TERNILO_STDIO_DIRECTORY", self.directory.path())
            .env("TERNILO_STDIO_PROTOCOL", protocol)
            .env("TERNILO_STDIO_MODE", mode)
            .env(
                "TERNILO_STDIO_MCP_VERSION",
                rmcp::model::ProtocolVersion::LATEST.to_string(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    pub(crate) async fn ready(&self, marker: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.directory.path().join(marker).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("stdio fixture did not reach its readiness marker");
    }

    pub(crate) fn calls(&self) -> usize {
        std::fs::read(self.directory.path().join("calls"))
            .unwrap_or_default()
            .len()
    }

    pub(crate) async fn assert_stopped(&self) {
        let pid = std::fs::read_to_string(self.directory.path().join("writer"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while process_running(pid) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("owned stdio descendant survived cleanup");
        let before = std::fs::read(self.directory.path().join("writes")).unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            std::fs::read(self.directory.path().join("writes")).unwrap(),
            before
        );
    }
}

impl Drop for StdioFixture {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.directory.path().join("leader"))
            && let Ok(pid) = pid.parse::<u32>()
        {
            OwnedProcessGroup::new(Some(pid)).kill();
        }
    }
}

fn process_running(pid: i32) -> bool {
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
        return false;
    }
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        && let Some((_, fields)) = stat.rsplit_once(')')
        && matches!(fields.split_whitespace().next(), Some("Z" | "X"))
    {
        return false;
    }
    true
}

const SERVER: &str = r#"
printf '%s' "$$" > "$TERNILO_STDIO_DIRECTORY/leader"
/bin/sh -c 'trap "" TERM; printf "%s" "$$" > "$1/writer"; while :; do printf x >> "$1/writes"; sleep 0.02; done' writer "$TERNILO_STDIO_DIRECTORY" &
while [ ! -f "$TERNILO_STDIO_DIRECTORY/writes" ]; do sleep 0.01; done
touch "$TERNILO_STDIO_DIRECTORY/ready"
case "$TERNILO_STDIO_MODE" in
  hang_startup) sleep 30; exit 0 ;;
  exit_startup) exit 0 ;;
esac
while :; do
  if [ "$TERNILO_STDIO_PROTOCOL" = mcp ]; then
    IFS= read -r body || exit 0
  else
    IFS= read -r header || exit 0
    length=$(printf '%s' "$header" | tr -d '\r' | sed 's/Content-Length: //')
    IFS= read -r blank || exit 0
    body=$(dd bs=1 count="$length" 2>/dev/null)
  fi
  case "$body" in *'"method":"exit"'*) exit 0 ;; esac
  id=$(printf '%s' "$body" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -n "$id" ] || continue
  called=false
  case "$body" in
    *'"method":"initialize"'*)
      if [ "$TERNILO_STDIO_PROTOCOL" = mcp ]; then
        result="{\"protocolVersion\":\"$TERNILO_STDIO_MCP_VERSION\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}"
      else result='{"capabilities":{}}'; fi ;;
    *'"method":"tools/list"'*)
      printf x >> "$TERNILO_STDIO_DIRECTORY/lists"
      result='{"tools":[{"name":"echo","description":"fixture","inputSchema":{"type":"object"}}]}'
      if [ -f "$TERNILO_STDIO_DIRECTORY/request" ]; then
        case "$TERNILO_STDIO_MODE" in
          list_changed|list_changed_during_call)
            result='{"tools":[{"name":"echo_v2","description":"updated fixture","inputSchema":{"type":"object","properties":{"value":{"type":"string"}}}}]}' ;;
          list_changed_invalid)
            result='{"tools":[{"name":"echo","inputSchema":{"type":"object"}},{"name":"echo","inputSchema":{"type":"object"}}]}' ;;
        esac
      fi ;;
    *'"method":"shutdown"'*) result=null ;;
    *)
      called=true
      touch "$TERNILO_STDIO_DIRECTORY/request"
      printf x >> "$TERNILO_STDIO_DIRECTORY/calls"
      printf '%s' "$body" > "$TERNILO_STDIO_DIRECTORY/last_request"
      case "$TERNILO_STDIO_MODE" in
        hang_call) sleep 30; exit 0 ;;
        fail_call) exit 0 ;;
        list_changed_during_call)
          printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}'
          touch "$TERNILO_STDIO_DIRECTORY/notification"
          while [ ! -f "$TERNILO_STDIO_DIRECTORY/release" ]; do sleep 0.01; done ;;
      esac
      if [ "$TERNILO_STDIO_PROTOCOL" = mcp ]; then
        result='{"content":[{"type":"text","text":"fixture result"}]}'
      else result='{"value":"fixture result"}'; fi ;;
  esac
  reply="{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$result}"
  if [ "$TERNILO_STDIO_PROTOCOL" = mcp ]; then printf '%s\n' "$reply"
  else printf 'Content-Length: %s\r\n\r\n%s' "${#reply}" "$reply"; fi
  if [ "$TERNILO_STDIO_PROTOCOL" = mcp ] && [ -f "$TERNILO_STDIO_DIRECTORY/request" ]; then
    case "$TERNILO_STDIO_MODE:$body" in
      list_changed:*'"method":"tools/call"'*|list_changed_invalid:*'"method":"tools/call"'*)
        printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}' ;;
    esac
  fi
  if [ "$TERNILO_STDIO_MODE" = exit_after_reply ] && [ "$called" = true ]; then exit 0; fi
done
"#;
