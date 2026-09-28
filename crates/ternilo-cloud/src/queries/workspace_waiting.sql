WITH pending AS (
    SELECT run.tenant_id,run.user_id,run.session_id,run.run_id,run.created_at_ms,run.queue_wait_reason,
        CASE WHEN EXISTS (
            SELECT 1 FROM cloud_workspace_occupancy occupancy
            WHERE occupancy.storage_id=$1 AND occupancy.tenant_id=run.tenant_id
                AND occupancy.workspace_id=run.workspace_id AND occupancy.state<>'released'
                AND (occupancy.state<>'held' OR occupancy.family_id<>family.family_id
                    OR NOT EXISTS (
                        SELECT 1 FROM cloud_runs holder
                        JOIN cloud_workers worker ON worker.worker_id=occupancy.worker_id
                            AND worker.generation=occupancy.worker_generation
                        JOIN cloud_worker_credentials credential ON credential.worker_id=worker.worker_id
                            AND credential.revoked_at_ms IS NULL
                        WHERE holder.tenant_id=occupancy.tenant_id AND holder.run_id=occupancy.run_id
                            AND holder.lease_token=occupancy.lease_token AND holder.lease_owner=occupancy.worker_id
                            AND holder.state IN ('leased','running','cancel_requested')
                            AND holder.lease_expires_at_ms>$2 AND worker.lease_expires_at_ms>$2))
        ) THEN 'workspace' ELSE 'capacity' END AS next_reason
    FROM cloud_runs run
    JOIN cloud_tenant_storage storage ON storage.tenant_id=run.tenant_id AND storage.storage_id=$1
    JOIN cloud_sessions session ON session.tenant_id=run.tenant_id AND session.session_id=run.session_id
        AND session.user_id=run.user_id AND session.state='queued' AND session.current_run_id IS NULL
    JOIN cloud_execution_families family ON family.tenant_id=run.tenant_id AND family.session_id=run.session_id
        AND family.owner_user_id=run.user_id AND family.workspace_id=run.workspace_id
    JOIN cloud_session_submissions submission ON submission.tenant_id=run.tenant_id
        AND submission.run_id=run.run_id AND submission.placement='running'
    JOIN cloud_session_inboxes inbox ON inbox.tenant_id=run.tenant_id AND inbox.session_id=run.session_id
        AND inbox.user_id=run.user_id AND inbox.paused=0
    WHERE run.state='queued'
)
SELECT tenant_id,user_id,session_id,run_id,next_reason FROM pending
WHERE COALESCE(queue_wait_reason,'')<>next_reason
ORDER BY created_at_ms,tenant_id,run_id LIMIT 32
