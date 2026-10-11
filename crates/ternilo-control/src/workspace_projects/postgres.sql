ALTER TABLE control_workspace_projects ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_workspace_projects_scope ON control_workspace_projects
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
REVOKE ALL ON control_workspace_projects FROM PUBLIC;
ALTER VIEW control_project_workspace_access SET (security_invoker = true);
REVOKE ALL ON control_project_workspace_access FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_workspace_projects TO ternilo_runtime;
    GRANT SELECT ON control_project_workspace_access TO ternilo_runtime;
END IF;
END $$;
