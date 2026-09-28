SELECT execution.* FROM cloud_run_execution execution
WHERE execution.storage_id=$1 AND execution.phase IN ('claimed','active','parked','resume_pending','cleanup')
AND NOT EXISTS (
    SELECT 1 FROM cloud_runs run
    JOIN cloud_workers worker ON worker.worker_id=run.lease_owner
    JOIN cloud_worker_credentials credential ON credential.worker_id=worker.worker_id AND credential.revoked_at_ms IS NULL
    WHERE run.tenant_id=execution.tenant_id AND run.run_id=execution.run_id
    AND run.lease_token=execution.lease_token AND run.lease_owner=execution.worker_id
    AND worker.generation=execution.worker_generation AND worker.lease_expires_at_ms>$2
    AND run.lease_expires_at_ms>$2 AND run.state IN ('leased','running','cancel_requested')
    AND ((execution.phase='claimed' AND run.state='leased' AND execution.writer_fencing_token=0)
        OR (execution.phase<>'claimed' AND run.state IN ('running','cancel_requested') AND EXISTS (
            SELECT 1 FROM cloud_session_writer_leases writer
            WHERE writer.tenant_id=run.tenant_id AND writer.session_id=run.session_id
            AND writer.run_id=run.run_id AND writer.lease_owner=run.lease_owner
            AND writer.fencing_token=execution.writer_fencing_token AND writer.expires_at_ms>$2)))
)
ORDER BY execution.updated_at_ms,execution.tenant_id,execution.run_id LIMIT 64
