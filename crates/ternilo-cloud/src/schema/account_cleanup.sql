CREATE INDEX cloud_runs_actor_cleanup
ON cloud_runs(actor_user_id, state, tenant_id, session_id, run_id);
