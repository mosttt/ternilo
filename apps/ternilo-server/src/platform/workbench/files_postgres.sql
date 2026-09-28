WITH sessions AS (
    SELECT session.tenant_id, session.session_id, session.workspace_id,
           session.user_id AS owner_id, session.title AS session_title,
           CASE WHEN session.archived_at_ms IS NULL THEN 0 ELSE 1 END AS session_archived,
           workspace.name AS workspace_name, CAST(NULL AS TEXT) AS executor_id, CAST(NULL AS TEXT) AS node_session_id
    FROM cloud_sessions AS session
    JOIN control_workspaces AS workspace ON workspace.tenant_id=session.tenant_id AND workspace.workspace_id=session.workspace_id
    WHERE session.tenant_id=$1
    UNION ALL
    SELECT session.tenant_id, session.session_id, session.workspace_id,
           session.owner_user_id AS owner_id, (CAST(session.metadata_json AS JSONB)#>>'{title}') AS session_title,
           CASE WHEN (CAST(session.metadata_json AS JSONB)#>>'{archived_at_ms}') IS NULL THEN 0 ELSE 1 END AS session_archived,
           workspace.name AS workspace_name, session.executor_id, session.node_session_id
    FROM control_edge_sessions AS session
    JOIN control_workspaces AS workspace ON workspace.tenant_id=session.tenant_id AND workspace.workspace_id=session.workspace_id
    WHERE session.tenant_id=$1
), candidates AS (
    SELECT * FROM sessions AS session
    WHERE (session.owner_id=$2 OR EXISTS (
          SELECT 1 FROM control_project_workspace_access p WHERE p.tenant_id=session.tenant_id
            AND p.workspace_id=session.workspace_id AND p.user_id=$2) OR EXISTS (
          SELECT 1 FROM control_resource_shares AS grant_record
          WHERE grant_record.tenant_id=session.tenant_id AND grant_record.grantee_user_id=$2
            AND ((grant_record.resource_kind='session' AND grant_record.resource_id=session.session_id)
              OR (grant_record.resource_kind='workspace' AND grant_record.resource_id=session.workspace_id)))
        OR EXISTS (
          SELECT 1 FROM control_resource_group_shares AS grant_record
          JOIN control_permission_group_members AS member ON member.tenant_id=grant_record.tenant_id AND member.group_id=grant_record.group_id
          WHERE grant_record.tenant_id=session.tenant_id AND member.user_id=$2
            AND ((grant_record.resource_kind='session' AND grant_record.resource_id=session.session_id)
              OR (grant_record.resource_kind='workspace' AND grant_record.resource_id=session.workspace_id)))
        OR EXISTS (
          SELECT 1 FROM control_resource_fork_group_sources AS source
          JOIN control_resource_group_shares AS grant_record ON grant_record.tenant_id=source.tenant_id
            AND grant_record.resource_kind=source.source_resource_kind AND grant_record.resource_id=source.source_resource_id AND grant_record.group_id=source.group_id
          JOIN control_permission_group_members AS member ON member.tenant_id=source.tenant_id AND member.group_id=source.group_id AND member.user_id=source.user_id
          WHERE source.tenant_id=session.tenant_id AND source.session_id=session.session_id AND source.user_id=$2))
      AND (CAST($3 AS TEXT) IS NULL OR session.session_id=$3)
      AND (CAST($4 AS TEXT) IS NULL OR session.workspace_id=$4)
), session_events AS (
    SELECT session.*, event.seq AS event_seq, event.event AS payload
    FROM candidates AS session JOIN cloud_session_events AS event
      ON event.tenant_id=session.tenant_id AND event.session_id=session.session_id
    WHERE session.executor_id IS NULL
    UNION ALL
    SELECT session.*, event.seq AS event_seq, event.event_json AS payload
    FROM candidates AS session JOIN control_edge_events AS event
      ON event.tenant_id=session.tenant_id AND event.executor_id=session.executor_id AND event.session_id=session.node_session_id
), event_files AS (
    SELECT session_id, session_title, session_archived, workspace_id, workspace_name, executor_id, event_seq,
           CAST(COALESCE((CAST(payload AS JSONB)#>>'{source,created_at_ms}'), (CAST(payload AS JSONB)#>>'{occurred_at_ms}')) AS BIGINT) AS occurred_at_ms,
           (CAST(payload AS JSONB)#>>'{run_id}') AS run_id,
           CASE WHEN (CAST(payload AS JSONB)#>>'{type}')='user_message' THEN 'upload' ELSE 'generated' END AS kind,
           CASE WHEN (CAST(payload AS JSONB)#>>'{type}')='deliverable_produced' THEN (CAST(payload AS JSONB)#>>'{path}') ELSE CAST(NULL AS TEXT) END AS path,
           (CAST(attachment.value AS JSONB)#>>'{name}') AS name, (CAST(attachment.value AS JSONB)#>>'{media_type}') AS media_type,
           CAST(attachment.ordinal - 1 AS BIGINT) AS attachment_index,
           CASE WHEN (CAST(payload AS JSONB)#>>'{type}')='deliverable_produced' THEN 'generated-' || CAST(event_seq AS TEXT) || '-0'
                WHEN (CAST(payload AS JSONB)#>>'{source,submission_id}') IS NOT NULL THEN 'submission-' || (CAST(payload AS JSONB)#>>'{source,submission_id}') || '-' || CAST(CAST(attachment.ordinal - 1 AS BIGINT) AS TEXT)
                ELSE 'upload-' || CAST(event_seq AS TEXT) || '-' || CAST(CAST(attachment.ordinal - 1 AS BIGINT) AS TEXT) END AS file_id
    FROM session_events CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN (CAST(payload AS JSONB)->>'type')='user_message' THEN COALESCE(CAST(payload AS JSONB)->'attachments', '[]'::jsonb) ELSE jsonb_build_array(CAST(payload AS JSONB)->'attachment') END) WITH ORDINALITY AS attachment(value, ordinal)
    WHERE (CAST(payload AS JSONB)#>>'{type}') IN ('user_message', 'deliverable_produced')
), accepted AS (
    SELECT session.session_id, session.session_title, session.session_archived, session.workspace_id, session.workspace_name, session.executor_id,
           upload.created_at_ms AS occurred_at_ms, upload.submitted_run_id AS run_id,
           (CAST(upload.attachment AS JSONB)#>>'{name}') AS name, (CAST(upload.attachment AS JSONB)#>>'{media_type}') AS media_type,
           upload.attachment_index, 'submission-' || upload.submission_id || '-' || CAST(upload.attachment_index AS TEXT) AS file_id
    FROM candidates AS session JOIN cloud_session_uploads AS upload
      ON upload.tenant_id=session.tenant_id AND upload.session_id=session.session_id
    WHERE session.executor_id IS NULL
    UNION ALL
    SELECT session.session_id, session.session_title, session.session_archived, session.workspace_id, session.workspace_name, session.executor_id,
           upload.created_at_ms AS occurred_at_ms, upload.submitted_run_id AS run_id,
           upload.name, upload.media_type,
           upload.attachment_index, 'submission-' || upload.submission_id || '-' || CAST(upload.attachment_index AS TEXT) AS file_id
    FROM candidates AS session JOIN control_edge_session_uploads AS upload
      ON upload.tenant_id=session.tenant_id AND upload.executor_id=session.executor_id AND upload.session_id=session.node_session_id
), files AS (
    SELECT accepted.session_id, accepted.session_title, accepted.session_archived, accepted.workspace_id, accepted.workspace_name, accepted.executor_id,
           (SELECT MAX(event_seq) FROM event_files WHERE event_files.session_id=accepted.session_id AND event_files.file_id=accepted.file_id) AS event_seq,
           accepted.occurred_at_ms, accepted.run_id, 'upload' AS kind, CAST(NULL AS TEXT) AS path,
           accepted.name, accepted.media_type, accepted.attachment_index, accepted.file_id
    FROM accepted
    UNION ALL
    SELECT * FROM event_files
    WHERE NOT EXISTS (SELECT 1 FROM accepted WHERE accepted.session_id=event_files.session_id AND accepted.file_id=event_files.file_id)
)
SELECT * FROM files
WHERE (CAST($5 AS TEXT) IS NULL OR kind=$5)
  AND strpos(lower(name), lower($6)) > 0
  AND (CAST($7 AS BIGINT) IS NULL OR (occurred_at_ms, session_id, file_id) < ($7, $8, $9))
ORDER BY occurred_at_ms DESC, session_id DESC, file_id DESC
LIMIT $10
