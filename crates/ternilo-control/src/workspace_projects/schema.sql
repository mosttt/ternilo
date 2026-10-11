CREATE TABLE control_workspace_projects (
    tenant_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, workspace_id),
    FOREIGN KEY (tenant_id, workspace_id)
        REFERENCES control_workspaces(tenant_id, workspace_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, project_id)
        REFERENCES control_projects(tenant_id, project_id) ON DELETE RESTRICT
);
CREATE INDEX control_workspace_projects_by_project
ON control_workspace_projects(tenant_id, project_id);

-- Names follow management ownership and project, under workspace-name locks.
DROP INDEX control_workspaces_registered_name;
CREATE INDEX control_workspaces_registered_name_lookup
ON control_workspaces(tenant_id, name) WHERE unregistered_at_ms IS NULL;

DROP VIEW control_project_workspace_access;
CREATE VIEW control_project_workspace_access AS
SELECT w.tenant_id, w.workspace_id, s.grantee_user_id AS user_id
FROM control_workspaces w
LEFT JOIN control_workspace_projects p ON p.tenant_id=w.tenant_id AND p.workspace_id=w.workspace_id
JOIN control_workspace_project_sharing i ON i.tenant_id=w.tenant_id AND i.workspace_id=w.workspace_id
JOIN control_project_user_shares s ON s.tenant_id=w.tenant_id AND s.project_id=COALESCE(p.project_id,w.project_id)
UNION
SELECT w.tenant_id, w.workspace_id, m.user_id
FROM control_workspaces w
LEFT JOIN control_workspace_projects p ON p.tenant_id=w.tenant_id AND p.workspace_id=w.workspace_id
JOIN control_workspace_project_sharing i ON i.tenant_id=w.tenant_id AND i.workspace_id=w.workspace_id
JOIN control_project_group_shares s ON s.tenant_id=w.tenant_id AND s.project_id=COALESCE(p.project_id,w.project_id)
JOIN control_permission_group_members m ON m.tenant_id=s.tenant_id AND m.group_id=s.group_id;
