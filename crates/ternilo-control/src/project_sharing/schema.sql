CREATE TABLE control_project_user_shares (
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    grantee_user_id TEXT NOT NULL,
    permissions_json TEXT NOT NULL,
    granted_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, project_id, grantee_user_id),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, grantee_user_id) REFERENCES control_memberships(tenant_id, user_id) ON DELETE CASCADE
);

CREATE TABLE control_project_group_shares (
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    permissions_json TEXT NOT NULL,
    granted_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, project_id, group_id),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, group_id) REFERENCES control_permission_groups(tenant_id, group_id) ON DELETE CASCADE
);

CREATE TABLE control_workspace_project_sharing (
    tenant_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    enabled_by TEXT NOT NULL REFERENCES control_users(user_id),
    enabled_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, workspace_id),
    FOREIGN KEY (tenant_id, workspace_id) REFERENCES control_workspaces(tenant_id, workspace_id) ON DELETE CASCADE
);

CREATE VIEW control_project_workspace_access AS
SELECT w.tenant_id, w.workspace_id, s.grantee_user_id AS user_id
FROM control_workspaces w
JOIN control_workspace_project_sharing i ON i.tenant_id=w.tenant_id AND i.workspace_id=w.workspace_id
JOIN control_project_user_shares s ON s.tenant_id=w.tenant_id AND s.project_id=w.project_id
UNION
SELECT w.tenant_id, w.workspace_id, m.user_id
FROM control_workspaces w
JOIN control_workspace_project_sharing i ON i.tenant_id=w.tenant_id AND i.workspace_id=w.workspace_id
JOIN control_project_group_shares s ON s.tenant_id=w.tenant_id AND s.project_id=w.project_id
JOIN control_permission_group_members m ON m.tenant_id=s.tenant_id AND m.group_id=s.group_id;
