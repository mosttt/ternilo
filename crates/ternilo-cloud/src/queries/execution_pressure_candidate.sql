SELECT wait.tenant_id,wait.owner_user_id,wait.child_session_id,wait.child_run_id
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
ORDER BY lineage.depth DESC,wait.child_run_id LIMIT 1
