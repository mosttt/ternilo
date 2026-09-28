SELECT CASE WHEN EXISTS (
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
) THEN CAST(1 AS BIGINT) ELSE CAST(0 AS BIGINT) END AS blocked
