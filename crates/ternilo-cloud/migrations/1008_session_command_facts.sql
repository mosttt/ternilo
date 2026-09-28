-- Session facts can be produced by the command plane while no model run
-- exists. They still carry a stable pseudo run_id for the shared event wire,
-- but are not owned by a cloud worker run.
ALTER TABLE cloud_session_events
    DROP CONSTRAINT cloud_session_events_tenant_id_run_id_fkey;
