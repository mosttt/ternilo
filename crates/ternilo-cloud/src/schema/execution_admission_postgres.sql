ALTER TABLE cloud_run_execution ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_run_wait_dependencies ENABLE ROW LEVEL SECURITY;
CREATE POLICY cloud_run_execution_owner_scope ON cloud_run_execution
USING (tenant_id = current_setting('ternilo.tenant_id', true) AND owner_user_id = current_setting('ternilo.user_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true) AND owner_user_id = current_setting('ternilo.user_id', true));
CREATE POLICY cloud_run_wait_dependencies_owner_scope ON cloud_run_wait_dependencies
USING (tenant_id = current_setting('ternilo.tenant_id', true) AND owner_user_id = current_setting('ternilo.user_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true) AND owner_user_id = current_setting('ternilo.user_id', true));

-- Cross-owner coordination reveals scoped scheduling metadata only; mutations recheck the canonical run.
CREATE FUNCTION ternilo_cloud_execution_usage(p_tenant TEXT,p_worker TEXT)
RETURNS TABLE(tenant_active BIGINT,worker_active BIGINT,worker_resident BIGINT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=public,pg_temp
AS $$ SELECT
    (SELECT COUNT(*) FROM cloud_run_execution WHERE tenant_id=p_tenant AND phase IN ('claimed','active')),
    (SELECT COUNT(*) FROM cloud_run_execution WHERE worker_id=p_worker AND phase IN ('claimed','active')),
    (SELECT COUNT(*) FROM cloud_run_execution WHERE worker_id=p_worker AND phase<>'released') $$;
CREATE FUNCTION ternilo_cloud_execution_entry(p_worker TEXT,p_tenant TEXT,p_run TEXT,p_lease BIGINT)
RETURNS SETOF cloud_run_execution LANGUAGE sql STABLE SECURITY DEFINER SET search_path=public,pg_temp
AS $$ SELECT * FROM cloud_run_execution WHERE worker_id=p_worker AND tenant_id=p_tenant AND run_id=p_run AND lease_token=p_lease $$;
CREATE FUNCTION ternilo_cloud_execution_invalid(p_storage TEXT,p_now BIGINT)
RETURNS SETOF cloud_run_execution LANGUAGE sql STABLE SECURITY DEFINER SET search_path=public,pg_temp
AS $$ SELECT execution.* FROM cloud_run_execution execution
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
ORDER BY execution.updated_at_ms,execution.tenant_id,execution.run_id LIMIT 64 $$;
CREATE FUNCTION ternilo_cloud_execution_worker_summary(p_worker TEXT)
RETURNS TABLE(residents BIGINT,nonparked BIGINT) LANGUAGE sql STABLE SECURITY DEFINER SET search_path=public,pg_temp
AS $$ SELECT COUNT(*) AS residents,
    COALESCE(SUM(CASE WHEN phase<>'parked' THEN 1 ELSE 0 END),0) AS nonparked
FROM cloud_run_execution WHERE worker_id=$1 AND phase<>'released' $$;
CREATE FUNCTION ternilo_cloud_execution_pressure_barrier(p_worker TEXT)
RETURNS TABLE(blocked BIGINT) LANGUAGE sql STABLE SECURITY DEFINER SET search_path=public,pg_temp
AS $$ SELECT CASE WHEN EXISTS (
    SELECT 1 FROM cloud_run_execution parent WHERE parent.worker_id=$1 AND parent.phase='parked'
    AND NOT EXISTS (SELECT 1 FROM cloud_run_wait_dependencies wait
        WHERE wait.tenant_id=parent.tenant_id AND wait.parent_run_id=parent.run_id
        AND wait.parent_lease_token=parent.lease_token AND wait.activity_revision=parent.activity_revision)
) OR EXISTS (
    SELECT 1 FROM cloud_run_wait_dependencies wait
    JOIN cloud_run_execution parent ON parent.tenant_id=wait.tenant_id AND parent.run_id=wait.parent_run_id
        AND parent.lease_token=wait.parent_lease_token AND parent.activity_revision=wait.activity_revision
    LEFT JOIN cloud_runs child ON child.tenant_id=wait.tenant_id AND child.run_id=wait.child_run_id
    WHERE parent.worker_id=$1 AND parent.phase='parked'
    AND (child.run_id IS NULL OR child.state NOT IN ('queued','leased','running','cancel_requested')
        OR (child.state<>'queued' AND NOT EXISTS (
            SELECT 1 FROM cloud_run_execution resident WHERE resident.tenant_id=child.tenant_id
            AND resident.run_id=child.run_id AND resident.lease_token=child.lease_token
            AND resident.worker_id=$1 AND resident.worker_generation=parent.worker_generation AND resident.phase='parked')))
) THEN CAST(1 AS BIGINT) ELSE CAST(0 AS BIGINT) END AS blocked $$;
CREATE FUNCTION ternilo_cloud_execution_pressure_candidate(p_worker TEXT,p_now BIGINT)
RETURNS TABLE(tenant_id TEXT,owner_user_id TEXT,child_session_id TEXT,child_run_id TEXT) LANGUAGE sql STABLE SECURITY DEFINER SET search_path=public,pg_temp
AS $$ SELECT wait.tenant_id,wait.owner_user_id,wait.child_session_id,wait.child_run_id
FROM cloud_run_wait_dependencies wait
JOIN cloud_run_execution parent ON parent.tenant_id=wait.tenant_id AND parent.run_id=wait.parent_run_id
    AND parent.lease_token=wait.parent_lease_token AND parent.activity_revision=wait.activity_revision
JOIN cloud_run_lineage lineage ON lineage.tenant_id=wait.tenant_id AND lineage.run_id=wait.child_run_id
JOIN cloud_runs child ON child.tenant_id=wait.tenant_id AND child.run_id=wait.child_run_id
JOIN cloud_session_submissions submission ON submission.tenant_id=child.tenant_id AND submission.run_id=child.run_id
JOIN cloud_session_inboxes inbox ON inbox.tenant_id=submission.tenant_id AND inbox.user_id=submission.user_id
    AND inbox.session_id=submission.session_id
WHERE parent.worker_id=$1 AND parent.phase='parked' AND child.state='queued'
    AND submission.placement='running' AND inbox.paused=0 AND child.available_at_ms<=$2
    AND EXISTS (
        SELECT 1 FROM cloud_execution_families family
        JOIN cloud_workspace_occupancy occupancy ON occupancy.tenant_id=family.tenant_id
            AND occupancy.workspace_id=family.workspace_id AND occupancy.family_id=family.family_id
        WHERE family.tenant_id=child.tenant_id AND family.session_id=child.session_id
            AND family.owner_user_id=child.user_id AND family.workspace_id=child.workspace_id
            AND occupancy.storage_id=parent.storage_id AND occupancy.worker_id=parent.worker_id
            AND occupancy.worker_generation=parent.worker_generation
            AND occupancy.run_id=parent.run_id AND occupancy.lease_token=parent.lease_token
            AND occupancy.state='held'
    )
ORDER BY lineage.depth DESC,wait.child_run_id LIMIT 1 $$;
REVOKE ALL ON FUNCTION ternilo_cloud_execution_usage(TEXT,TEXT), ternilo_cloud_execution_entry(TEXT,TEXT,TEXT,BIGINT), ternilo_cloud_execution_invalid(TEXT,BIGINT), ternilo_cloud_execution_worker_summary(TEXT), ternilo_cloud_execution_pressure_barrier(TEXT), ternilo_cloud_execution_pressure_candidate(TEXT,BIGINT) FROM PUBLIC;
DO $$ BEGIN IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
GRANT SELECT,INSERT,UPDATE,DELETE ON cloud_run_execution,cloud_run_wait_dependencies TO ternilo_runtime;
GRANT EXECUTE ON FUNCTION ternilo_cloud_execution_usage(TEXT,TEXT), ternilo_cloud_execution_entry(TEXT,TEXT,TEXT,BIGINT), ternilo_cloud_execution_invalid(TEXT,BIGINT), ternilo_cloud_execution_worker_summary(TEXT), ternilo_cloud_execution_pressure_barrier(TEXT), ternilo_cloud_execution_pressure_candidate(TEXT,BIGINT) TO ternilo_runtime;
END IF; END $$;
