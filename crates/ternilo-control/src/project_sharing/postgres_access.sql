ALTER TABLE control_project_user_shares ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_project_user_shares_scope ON control_project_user_shares
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

ALTER TABLE control_project_group_shares ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_project_group_shares_scope ON control_project_group_shares
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

ALTER TABLE control_workspace_project_sharing ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_workspace_project_sharing_scope ON control_workspace_project_sharing
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

ALTER VIEW control_project_workspace_access SET (security_invoker = true);
REVOKE ALL ON control_project_user_shares, control_project_group_shares,
    control_workspace_project_sharing, control_project_workspace_access FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_project_user_shares,
        control_project_group_shares, control_workspace_project_sharing TO ternilo_runtime;
    GRANT SELECT ON control_project_workspace_access TO ternilo_runtime;
END IF;
END $$;
