REVOKE ALL ON cloud_runtime_control FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_claim_gate() FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_run_scopes(TEXT,TEXT,BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_command_scopes(TEXT,TEXT,BIGINT,BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_telemetry_scopes(TEXT,TEXT,BIGINT,BIGINT) FROM PUBLIC;
DO $$
BEGIN
    IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
        GRANT SELECT,INSERT ON cloud_run_lineage TO ternilo_runtime;
        GRANT SELECT,INSERT,UPDATE,DELETE ON cloud_runs,cloud_sessions,cloud_workers,
            cloud_session_commands,cloud_session_inboxes,cloud_session_submissions,
            cloud_session_writer_leases,cloud_session_events,cloud_session_uploads,
            cloud_session_questions,cloud_attachment_objects,cloud_agent_team_tasks,
            cloud_agent_team_task_dependencies,cloud_agent_team_messages,
            cloud_session_telemetry,cloud_telemetry_outbox TO ternilo_runtime;
        GRANT EXECUTE ON FUNCTION ternilo_cloud_claim_gate(), ternilo_cloud_run_scopes(TEXT,TEXT,BIGINT),
            ternilo_cloud_command_scopes(TEXT,TEXT,BIGINT,BIGINT),
            ternilo_cloud_telemetry_scopes(TEXT,TEXT,BIGINT,BIGINT) TO ternilo_runtime;
    END IF;
END;
$$;
