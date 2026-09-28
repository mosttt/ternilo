ALTER TABLE cloud_session_submissions ADD COLUMN batch_run_id TEXT;
CREATE INDEX cloud_session_submissions_batch
ON cloud_session_submissions (tenant_id, batch_run_id)
WHERE batch_run_id IS NOT NULL;
