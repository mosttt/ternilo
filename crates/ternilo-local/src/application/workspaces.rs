use super::{
    Attachment, DirectoryListing, HarnessError, LocalApplication, Path, Workspace, WorkspaceId,
    canonical_directory, create_directory, list_directory, now_ms,
};

impl LocalApplication {
    pub async fn resolve_attachment(
        &self,
        attachment: Attachment,
    ) -> Result<Attachment, HarnessError> {
        self.attachments.resolve_attachment(attachment).await
    }

    pub async fn add_workspace(&self, path: &str) -> Result<Workspace, HarnessError> {
        let canonical = canonical_directory(path).await?;
        let path = path_string(&canonical)?;
        if let Some(workspace) = self.state.workspace_by_path(&path).await {
            return Ok(workspace);
        }
        let now = now_ms()?;
        let workspace = Workspace {
            workspace_id: WorkspaceId::new(self.next_id("workspace")?),
            title: workspace_title(&canonical),
            path,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.state.insert_workspace(workspace.clone()).await?;
        self.invalidate(None, crate::LocalInvalidationCategory::Workbench, None);
        Ok(workspace)
    }

    pub async fn rename_workspace(
        &self,
        workspace_id: WorkspaceId,
        title: String,
    ) -> Result<Workspace, HarnessError> {
        let workspace = self
            .state
            .rename_workspace(&workspace_id, title, now_ms()?)
            .await?;
        self.invalidate(None, crate::LocalInvalidationCategory::Workbench, None);
        Ok(workspace)
    }

    /// Remove the sidebar registration only. Session logs and their captured
    /// workspace paths remain valid and continue to boot as ungrouped.
    pub async fn unregister_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Workspace, HarnessError> {
        let workspace = self.state.unregister_workspace(&workspace_id).await?;
        self.invalidate(None, crate::LocalInvalidationCategory::Workbench, None);
        Ok(workspace)
    }

    pub async fn list_directory(
        &self,
        path: Option<&str>,
    ) -> Result<DirectoryListing, HarnessError> {
        list_directory(path).await
    }

    pub async fn create_directory(&self, parent: &str, name: &str) -> Result<String, HarnessError> {
        create_directory(parent, name).await
    }
}

pub(super) fn workspace_title(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), str::to_owned)
}

fn path_string(path: &Path) -> Result<String, HarnessError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        HarnessError::invalid(format!(
            "workspace path is not valid UTF-8: {}",
            path.display()
        ))
    })
}
