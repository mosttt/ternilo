ALTER TABLE cloud_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_run_lineage ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_inboxes ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_submissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_writer_leases ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_uploads ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_questions ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_attachment_objects ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_commands ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_agent_team_tasks ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_agent_team_task_dependencies ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_agent_team_messages ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_telemetry ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_telemetry_outbox ENABLE ROW LEVEL SECURITY;

CREATE POLICY cloud_runs_scope ON cloud_runs
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY cloud_run_lineage_owner_scope ON cloud_run_lineage
USING (tenant_id = current_setting('ternilo.tenant_id', true)
    AND owner_user_id = current_setting('ternilo.user_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true)
    AND owner_user_id = current_setting('ternilo.user_id', true));

CREATE POLICY cloud_sessions_scope ON cloud_sessions
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY cloud_session_inboxes_owner_scope ON cloud_session_inboxes
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_session_submissions_owner_scope ON cloud_session_submissions
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_session_writer_leases_scope ON cloud_session_writer_leases
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY cloud_session_events_scope ON cloud_session_events
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));


CREATE POLICY cloud_session_questions_owner_scope ON cloud_session_questions
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_attachment_objects_tenant_scope ON cloud_attachment_objects
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY cloud_session_commands_owner_scope ON cloud_session_commands
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_agent_team_tasks_owner_scope ON cloud_agent_team_tasks
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_agent_team_dependencies_owner_scope
ON cloud_agent_team_task_dependencies
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_agent_team_messages_owner_scope ON cloud_agent_team_messages
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_session_telemetry_owner_scope ON cloud_session_telemetry
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_telemetry_outbox_owner_scope ON cloud_telemetry_outbox
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

-- Read-only cross-owner discovery. Each mutation rechecks its scope and lease in Rust.
CREATE FUNCTION ternilo_cloud_run_scopes(p_operation TEXT,p_worker TEXT,p_now_ms BIGINT)
RETURNS TABLE(tenant_id TEXT,user_id TEXT,session_id TEXT,run_id TEXT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=public,pg_temp AS $$
SELECT run.tenant_id, run.user_id, run.session_id, run.run_id FROM cloud_runs
        AS run WHERE ($1='claim' AND (SELECT claims_paused FROM cloud_runtime_control
        WHERE singleton=1)=0 AND ((run.state='queued' AND run.available_at_ms <= $3)
        OR (run.state='leased' AND run.lease_expires_at_ms <= $3 AND run.attempt<run.max_attempts)))
        AND (NOT EXISTS(SELECT 1 FROM cloud_tenant_storage WHERE tenant_id=run.tenant_id)
            OR EXISTS(SELECT 1 FROM cloud_tenant_storage location JOIN cloud_worker_credentials credential ON credential.storage_id=location.storage_id WHERE location.tenant_id=run.tenant_id AND credential.worker_id=$2 AND credential.revoked_at_ms IS NULL))
        AND (SELECT COUNT(*) FROM cloud_run_execution WHERE tenant_id=run.tenant_id AND phase IN ('claimed','active')) < (SELECT max_concurrent_runs FROM control_quotas WHERE tenant_id=run.tenant_id)
        AND EXISTS(SELECT 1 FROM cloud_session_submissions submission JOIN cloud_session_inboxes inbox ON inbox.tenant_id=submission.tenant_id AND inbox.user_id=submission.user_id AND inbox.session_id=submission.session_id WHERE submission.tenant_id=run.tenant_id AND submission.run_id=run.run_id AND submission.placement='running' AND inbox.paused=0)
        AND NOT EXISTS (
            SELECT 1 FROM cloud_workspace_occupancy occupancy
            WHERE occupancy.tenant_id=run.tenant_id AND occupancy.workspace_id=run.workspace_id
                AND occupancy.storage_id=(SELECT storage_id FROM cloud_worker_credentials WHERE worker_id=$2)
                AND occupancy.state<>'released'
                AND (occupancy.state<>'held' OR occupancy.worker_id<>$2
                    OR occupancy.worker_generation<>(SELECT generation FROM cloud_workers WHERE worker_id=$2)
                    OR NOT EXISTS (SELECT 1 FROM cloud_execution_families family
                        WHERE family.tenant_id=run.tenant_id AND family.session_id=run.session_id
                            AND family.owner_user_id=run.user_id AND family.workspace_id=run.workspace_id
                            AND family.family_id=occupancy.family_id)
                    OR NOT EXISTS (SELECT 1 FROM cloud_runs holder
                        WHERE holder.tenant_id=occupancy.tenant_id AND holder.run_id=occupancy.run_id
                            AND holder.lease_token=occupancy.lease_token AND holder.lease_owner=occupancy.worker_id
                            AND holder.state IN ('leased','running','cancel_requested') AND holder.lease_expires_at_ms>$3)))
        OR ($1='reap' AND run.lease_expires_at_ms <= $3 AND (run.state IN ('running',
        'cancel_requested') OR (run.state='leased' AND run.attempt>=run.max_attempts)))
        OR ($1='drain' AND run.lease_owner=$2 AND run.state IN ('leased', 'running',
        'cancel_requested')) OR ($1='writers' AND EXISTS(SELECT 1 FROM cloud_session_writer_leases
        AS writer WHERE writer.tenant_id=run.tenant_id AND writer.run_id=run.run_id
        AND writer.expires_at_ms<=$3)) ORDER BY CASE WHEN $1='claim' AND EXISTS(SELECT 1 FROM cloud_run_wait_dependencies wait JOIN cloud_run_execution parent ON parent.tenant_id=wait.tenant_id AND parent.run_id=wait.parent_run_id AND parent.lease_token=wait.parent_lease_token WHERE wait.tenant_id=run.tenant_id AND wait.child_run_id=run.run_id AND parent.phase='parked') THEN 0 ELSE 1 END,
        run.priority DESC, run.available_at_ms,run.created_at_ms,run.tenant_id,run.run_id LIMIT CASE WHEN $1='claim' THEN 64 ELSE NULL END
$$;

-- The operator's claim barrier serializes with dispatch until the transaction commits.
CREATE FUNCTION ternilo_cloud_claim_gate() RETURNS BIGINT
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path=public,pg_temp AS $$
    SELECT 1-claims_paused FROM cloud_runtime_control WHERE singleton=1 FOR SHARE
$$;

CREATE POLICY cloud_session_uploads_scope ON cloud_session_uploads
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
