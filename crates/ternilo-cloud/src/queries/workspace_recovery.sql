SELECT execution.* FROM cloud_run_execution execution
JOIN cloud_workspace_occupancy occupancy ON occupancy.tenant_id=execution.tenant_id
    AND occupancy.run_id=execution.run_id AND occupancy.lease_token=execution.lease_token
JOIN cloud_workspace_epochs epoch ON epoch.storage_id=occupancy.storage_id
    AND epoch.tenant_id=occupancy.tenant_id AND epoch.workspace_id=occupancy.workspace_id
    AND epoch.occupation_epoch=occupancy.occupation_epoch
WHERE execution.storage_id=$1 AND execution.phase IN ('lost','cleanup')
    AND occupancy.state IN ('held','cleanup')
    AND (execution.tenant_id,execution.run_id,execution.lease_token)>($2,$3,$4)
    AND NOT EXISTS (SELECT 1 FROM cloud_runs run WHERE run.tenant_id=execution.tenant_id
        AND run.run_id=execution.run_id AND run.lease_token=execution.lease_token
        AND run.state IN ('leased','running','cancel_requested') AND run.lease_expires_at_ms>$5)
ORDER BY execution.tenant_id,execution.run_id,execution.lease_token LIMIT 32
