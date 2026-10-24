use super::LocalApplication;
use std::sync::Arc;
use ternilo_protocol::HarnessError;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard};

impl LocalApplication {
    pub(super) fn accept_update_safe_work(&self) -> Result<OwnedRwLockReadGuard<()>, HarnessError> {
        Arc::clone(&self.maintenance).try_read_owned().map_err(|_| {
            HarnessError::unavailable(
                "this computer is installing a client update; retry after it reconnects",
            )
        })
    }

    /// Claim an idle host without waiting ahead of child tasks spawned by an active run.
    /// Keep this guard until the service has closed, or drop it to cancel maintenance.
    pub async fn try_client_update_maintenance(
        &self,
    ) -> Result<Option<OwnedRwLockWriteGuard<()>>, HarnessError> {
        let Ok(guard) = Arc::clone(&self.maintenance).try_write_owned() else {
            return Ok(None);
        };
        self.execution_resources.prune().await?;
        if let Some((id, owner)) = self.execution_resources.unobserved_owner().await {
            return Err(HarnessError::unavailable(format!(
                "client update blocked by an unobserved execution resource: {id}; session={}; run={}. The client restarted without a completion receipt. Check remaining processes before resolving the matching record in runtime/execution-resources.json",
                owner.session_id, owner.run_id,
            )));
        }
        if !self.execution_resources.owners().await.is_empty() {
            return Ok(None);
        }
        for session in self.state.snapshot().await.sessions {
            let inbox = self
                .session_inbox(session.identity.session_id.as_str())
                .await?;
            if inbox.active_run_id.is_some() || !inbox.items.is_empty() {
                return Ok(None);
            }
        }
        Ok(Some(guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_kernel::HostPolicy;
    use ternilo_protocol::RunLimits;

    #[tokio::test]
    async fn idle_claim_cannot_interrupt_a_run_or_admit_work_until_released() {
        let root = tempfile::tempdir().unwrap();
        let app = LocalApplication::open(
            crate::catalog().unwrap(),
            crate::local_profile(),
            HostPolicy::local(RunLimits::default()),
            root.path().to_owned(),
        )
        .await
        .unwrap();
        let active = app.accept_update_safe_work().unwrap();
        assert!(app.try_client_update_maintenance().await.unwrap().is_none());
        // A pending update must not queue a writer ahead of children needed by this run.
        let child = app.accept_update_safe_work().unwrap();
        drop(active);
        drop(child);
        let guard = app.try_client_update_maintenance().await.unwrap().unwrap();
        assert!(app.accept_update_safe_work().is_err());
        drop(guard);
        assert!(app.accept_update_safe_work().is_ok());
        app.close().await.unwrap();
    }

    #[tokio::test]
    async fn unobserved_resources_report_the_blocker_instead_of_waiting_forever() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let record = runtime.join("execution-resources.json");
        let bytes =
            br#"{"process-previous":{"session_id":"previous-session","run_id":"previous-run"}}"#;
        std::fs::write(&record, bytes).unwrap();
        let app = LocalApplication::open(
            crate::catalog().unwrap(),
            crate::local_profile(),
            HostPolicy::local(RunLimits::default()),
            root.path().to_owned(),
        )
        .await
        .unwrap();
        let Err(error) = app.try_client_update_maintenance().await else {
            panic!("unknown process state must not be treated as ordinary busy work");
        };
        assert!(error.message.contains("process-previous"));
        assert!(error.message.contains("previous-session"));
        assert_eq!(std::fs::read(&record).unwrap(), bytes);
        assert!(app.accept_update_safe_work().is_ok());
        app.close().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn idle_session_services_retain_directory_ownership_without_blocking_updates() {
        use ternilo_protocol::{PluginEntry, SessionServiceStatus};

        for protocol in ["mcp", "lsp"] {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace");
            std::fs::create_dir(&workspace).unwrap();
            let mut profile = crate::local_profile();
            profile.plugins.push(PluginEntry {
                id: "idle-service".to_owned(),
                kind: format!("ternilo.{protocol}.stdio"),
                enabled: true,
                config: serde_json::json!({
                    "server_name": "fixture", "command": "/bin/sh",
                    "args": ["-c", STDIO_SERVICE], "env": {"PROTOCOL": protocol},
                }),
            });
            let mut app = LocalApplication::open(
                crate::catalog().unwrap(),
                profile,
                HostPolicy::local(RunLimits::default()),
                root.path().join("data"),
            )
            .await
            .unwrap();
            app.directory_coordinator = crate::DirectoryCoordinator::new(root.path().join("locks"));
            let binding = app
                .add_workspace(workspace.to_str().unwrap())
                .await
                .unwrap();
            app.create_session(binding.workspace_id, Some("service".to_owned()), None)
                .await
                .unwrap();
            let id = format!("{protocol}:fixture");
            let service = app
                .start_session_service("service", id.clone())
                .await
                .unwrap();
            assert_eq!(service.status, SessionServiceStatus::Running);
            let outsider = app
                .directory_coordinator
                .bind("unrelated".to_owned(), workspace);
            assert!(outsider.try_acquire().await.unwrap().is_none());

            let guard = app
                .try_client_update_maintenance()
                .await
                .unwrap()
                .expect("an idle session service must not keep an update waiting forever");
            assert!(app.start_session_service("service", id).await.is_err());
            app.close().await.unwrap();
            assert!(outsider.try_acquire().await.unwrap().is_some());
            drop(guard);
        }
    }

    #[cfg(unix)]
    const STDIO_SERVICE: &str = r#"
while :; do
  if [ "$PROTOCOL" = mcp ]; then IFS= read -r body || exit 0
  else
    IFS= read -r header || exit 0
    length=$(printf '%s' "$header" | tr -d '\r' | sed 's/Content-Length: //')
    IFS= read -r blank || exit 0
    body=$(dd bs=1 count="$length" 2>/dev/null)
  fi
  case "$body" in *'"method":"exit"'*) exit 0 ;; esac
  id=$(printf '%s' "$body" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -n "$id" ] || continue
  case "$body" in
    *'"method":"initialize"'*)
      if [ "$PROTOCOL" = mcp ]; then
        version=$(printf '%s' "$body" | sed -n 's/.*"protocolVersion":"\([^"]*\)".*/\1/p')
        result="{\"protocolVersion\":\"$version\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}"
      else result='{"capabilities":{}}'; fi ;;
    *'"method":"tools/list"'*) result='{"tools":[]}' ;;
    *) result=null ;;
  esac
  reply="{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$result}"
  if [ "$PROTOCOL" = mcp ]; then printf '%s\n' "$reply"
  else printf 'Content-Length: %s\r\n\r\n%s' "${#reply}" "$reply"; fi
done
"#;
}
