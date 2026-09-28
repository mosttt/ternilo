-- Transactional wake-up hints for Control live WebSocket readers. Payloads
-- identify only the durable scope to re-read; no mutable state is trusted from
-- PostgreSQL NOTIFY itself.
CREATE FUNCTION ternilo_notify_cloud_live_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    row_value JSONB;
BEGIN
    row_value := CASE WHEN TG_OP = 'DELETE' THEN to_jsonb(OLD) ELSE to_jsonb(NEW) END;
    PERFORM pg_notify(
        'ternilo_cloud_live',
        json_build_object(
            'kind', TG_ARGV[0],
            'tenant_id', row_value->>'tenant_id',
            'user_id', COALESCE(row_value->>'user_id', row_value->>'owner_user_id'),
            'session_id', row_value->>'session_id'
        )::text
    );
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER cloud_live_inbox_change
AFTER INSERT OR UPDATE OR DELETE ON cloud_session_inboxes
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('inbox');

CREATE TRIGGER cloud_live_submission_change
AFTER INSERT OR UPDATE OR DELETE ON cloud_session_submissions
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('inbox');

CREATE TRIGGER cloud_live_question_change
AFTER INSERT OR UPDATE OR DELETE ON cloud_session_questions
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('questions');

CREATE TRIGGER cloud_live_team_task_change
AFTER INSERT OR UPDATE OR DELETE ON cloud_agent_team_tasks
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('agent_team');

CREATE TRIGGER cloud_live_team_dependency_change
AFTER INSERT OR UPDATE OR DELETE ON cloud_agent_team_task_dependencies
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('agent_team');

CREATE TRIGGER cloud_live_team_message_change
AFTER INSERT OR UPDATE OR DELETE ON cloud_agent_team_messages
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('agent_team');

CREATE TRIGGER cloud_live_session_insert_delete
AFTER INSERT OR DELETE ON cloud_sessions
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('session');

CREATE TRIGGER cloud_live_session_update
AFTER UPDATE ON cloud_sessions
FOR EACH ROW
WHEN (
    OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
    OR OLD.parent_session_id IS DISTINCT FROM NEW.parent_session_id
    OR OLD.subagent_metadata IS DISTINCT FROM NEW.subagent_metadata
    OR OLD.title IS DISTINCT FROM NEW.title
    OR OLD.archived_at_ms IS DISTINCT FROM NEW.archived_at_ms
    OR OLD.permissions IS DISTINCT FROM NEW.permissions
    OR OLD.route_id IS DISTINCT FROM NEW.route_id
    OR OLD.model IS DISTINCT FROM NEW.model
    OR OLD.reasoning_effort IS DISTINCT FROM NEW.reasoning_effort
    OR OLD.agent_preset IS DISTINCT FROM NEW.agent_preset
    OR OLD.profile_plugins IS DISTINCT FROM NEW.profile_plugins
    OR OLD.mode IS DISTINCT FROM NEW.mode
)
EXECUTE FUNCTION ternilo_notify_cloud_live_change('session');

CREATE TRIGGER cloud_live_session_activity_update
AFTER UPDATE ON cloud_sessions
FOR EACH ROW
WHEN (OLD.state IS DISTINCT FROM NEW.state)
EXECUTE FUNCTION ternilo_notify_cloud_live_change('activity');

CREATE TRIGGER cloud_live_workspace_change
AFTER INSERT OR UPDATE OR DELETE ON control_workspaces
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('workbench');

CREATE TRIGGER cloud_live_edge_session_change
AFTER INSERT OR UPDATE OR DELETE ON control_edge_sessions
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('workbench');

REVOKE ALL ON FUNCTION ternilo_notify_cloud_live_change() FROM PUBLIC;
