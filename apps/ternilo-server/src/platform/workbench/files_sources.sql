SELECT workspace.workspace_id, workspace.executor_id, 'workspace' AS resource_kind, workspace.workspace_id AS resource_id
FROM control_workspaces AS workspace
WHERE workspace.tenant_id=$1 AND workspace.executor_id IS NOT NULL
  AND (workspace.owner_user_id=$2 OR EXISTS (
    SELECT 1 FROM control_resource_shares AS grant_record
    WHERE grant_record.tenant_id=workspace.tenant_id AND grant_record.resource_kind='workspace'
      AND grant_record.resource_id=workspace.workspace_id AND grant_record.grantee_user_id=$2)
    OR EXISTS (
      SELECT 1 FROM control_resource_group_shares AS grant_record
      JOIN control_permission_group_members AS member ON member.tenant_id=grant_record.tenant_id AND member.group_id=grant_record.group_id
      WHERE grant_record.tenant_id=workspace.tenant_id AND grant_record.resource_kind='workspace'
        AND grant_record.resource_id=workspace.workspace_id AND member.user_id=$2))
  AND CAST($3 AS TEXT) IS NULL
  AND (CAST($4 AS TEXT) IS NULL OR workspace.workspace_id=$4)
UNION
SELECT session.workspace_id, session.executor_id, 'session' AS resource_kind, session.session_id AS resource_id
FROM control_edge_sessions AS session
WHERE session.tenant_id=$1
  AND (CAST($3 AS TEXT) IS NOT NULL OR EXISTS (
    SELECT 1 FROM control_resource_shares AS grant_record
    WHERE grant_record.tenant_id=session.tenant_id AND grant_record.resource_kind='session'
      AND grant_record.resource_id=session.session_id AND grant_record.grantee_user_id=$2)
    OR EXISTS (
      SELECT 1 FROM control_resource_group_shares AS grant_record
      JOIN control_permission_group_members AS member ON member.tenant_id=grant_record.tenant_id AND member.group_id=grant_record.group_id
      WHERE grant_record.tenant_id=session.tenant_id AND grant_record.resource_kind='session'
        AND grant_record.resource_id=session.session_id AND member.user_id=$2)
    OR EXISTS (
      SELECT 1 FROM control_resource_fork_group_sources AS source
      WHERE source.tenant_id=session.tenant_id AND source.session_id=session.session_id AND source.user_id=$2))
  AND (CAST($3 AS TEXT) IS NULL OR session.session_id=$3)
  AND (CAST($4 AS TEXT) IS NULL OR session.workspace_id=$4)
