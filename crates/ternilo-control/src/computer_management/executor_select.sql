SELECT e.executor_id,e.project_id,e.state,e.enrolled_at_ms,
    CASE WHEN edge.last_seen_at_ms IS NOT NULL AND (e.last_seen_at_ms IS NULL OR edge.last_seen_at_ms>e.last_seen_at_ms)
    THEN edge.last_seen_at_ms ELSE e.last_seen_at_ms END AS last_seen_at_ms,
    m.display_name,COALESCE(m.notes,'') AS notes,m.suspended_at_ms,m.removed_at_ms,COALESCE(m.revision,0) AS revision
    FROM control_executors e LEFT JOIN control_edge_executors edge
    ON edge.tenant_id=e.tenant_id AND edge.executor_id=e.executor_id LEFT JOIN control_computer_management m
    ON m.tenant_id=e.tenant_id AND m.executor_id=e.executor_id
