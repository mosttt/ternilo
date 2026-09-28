-- The global feed exposes identifiers and invalidation kinds, never event bodies.
CREATE TABLE cloud_live_changes (
    sequence BIGSERIAL PRIMARY KEY,
    kind TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    user_id TEXT,
    session_id TEXT,
    event_type TEXT
);
REVOKE ALL ON cloud_live_changes FROM PUBLIC;
DO $$ BEGIN
    IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
        GRANT SELECT ON cloud_live_changes TO ternilo_runtime;
    END IF;
END $$;

CREATE FUNCTION ternilo_notify_cloud_session_event()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
DECLARE
    change_sequence BIGINT;
    event_owner TEXT;
BEGIN
    SELECT user_id INTO event_owner FROM public.cloud_sessions
        WHERE tenant_id = NEW.tenant_id AND session_id = NEW.session_id;
    INSERT INTO public.cloud_live_changes (kind, tenant_id, user_id, session_id, event_type)
    VALUES ('event', NEW.tenant_id, event_owner, NEW.session_id, NEW.event::jsonb->>'type')
    RETURNING sequence INTO change_sequence;
    PERFORM pg_notify('ternilo_cloud_session_events', json_build_object(
        'sequence', change_sequence, 'tenant_id', NEW.tenant_id,
        'user_id', event_owner, 'session_id', NEW.session_id,
        'event_type', NEW.event::jsonb->>'type')::text);
    RETURN NEW;
END;
$$;
CREATE TRIGGER cloud_session_event_notify
AFTER INSERT ON cloud_session_events
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_session_event();
REVOKE ALL ON FUNCTION ternilo_notify_cloud_session_event() FROM PUBLIC;

CREATE FUNCTION ternilo_notify_cloud_live_change()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
DECLARE
    row_value JSONB;
    change_sequence BIGINT;
BEGIN
    row_value := CASE WHEN TG_OP = 'DELETE' THEN to_jsonb(OLD) ELSE to_jsonb(NEW) END;
    INSERT INTO public.cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES (TG_ARGV[0], row_value->>'tenant_id',
        COALESCE(row_value->>'user_id', row_value->>'owner_user_id'), row_value->>'session_id')
    RETURNING sequence INTO change_sequence;
    PERFORM pg_notify('ternilo_cloud_live', json_build_object(
        'sequence', change_sequence, 'kind', TG_ARGV[0],
        'tenant_id', row_value->>'tenant_id',
        'user_id', COALESCE(row_value->>'user_id', row_value->>'owner_user_id'),
        'session_id', row_value->>'session_id')::text);
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION ternilo_notify_cloud_live_change() FROM PUBLIC;

CREATE TRIGGER cloud_live_cloud_session_inboxes
AFTER INSERT OR UPDATE OR DELETE ON cloud_session_inboxes
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('inbox');

CREATE TRIGGER cloud_live_cloud_session_submissions
AFTER INSERT OR UPDATE OR DELETE ON cloud_session_submissions
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('inbox');

CREATE TRIGGER cloud_live_cloud_session_questions
AFTER INSERT OR UPDATE OR DELETE ON cloud_session_questions
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('questions');

CREATE TRIGGER cloud_live_cloud_agent_team_tasks
AFTER INSERT OR UPDATE OR DELETE ON cloud_agent_team_tasks
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('agent_team');

CREATE TRIGGER cloud_live_cloud_agent_team_task_dependencies
AFTER INSERT OR UPDATE OR DELETE ON cloud_agent_team_task_dependencies
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('agent_team');

CREATE TRIGGER cloud_live_cloud_agent_team_messages
AFTER INSERT OR UPDATE OR DELETE ON cloud_agent_team_messages
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('agent_team');

CREATE TRIGGER cloud_live_session_insert_delete
AFTER INSERT OR DELETE ON cloud_sessions
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('session');

CREATE TRIGGER cloud_live_session_update
AFTER UPDATE ON cloud_sessions
FOR EACH ROW WHEN (OLD.workspace_id IS DISTINCT FROM NEW.workspace_id OR OLD.parent_session_id IS DISTINCT FROM NEW.parent_session_id OR OLD.subagent_metadata IS DISTINCT FROM NEW.subagent_metadata OR OLD.title IS DISTINCT FROM NEW.title OR OLD.archived_at_ms IS DISTINCT FROM NEW.archived_at_ms OR OLD.permissions IS DISTINCT FROM NEW.permissions OR OLD.model_snapshot IS DISTINCT FROM NEW.model_snapshot OR OLD.reserved_model_tokens IS DISTINCT FROM NEW.reserved_model_tokens OR OLD.agent_preset IS DISTINCT FROM NEW.agent_preset OR OLD.profile_plugins IS DISTINCT FROM NEW.profile_plugins OR OLD.mode IS DISTINCT FROM NEW.mode)
EXECUTE FUNCTION ternilo_notify_cloud_live_change('session');

CREATE TRIGGER cloud_live_session_activity_update
AFTER UPDATE ON cloud_sessions
FOR EACH ROW WHEN (OLD.state IS DISTINCT FROM NEW.state OR OLD.execution IS DISTINCT FROM NEW.execution)
EXECUTE FUNCTION ternilo_notify_cloud_live_change('activity');

CREATE TRIGGER cloud_live_control_workspaces_insert_delete
AFTER INSERT OR DELETE ON control_workspaces
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('workbench');

CREATE TRIGGER cloud_live_control_workspaces_update
AFTER UPDATE ON control_workspaces
FOR EACH ROW WHEN (OLD.project_id IS DISTINCT FROM NEW.project_id OR OLD.owner_user_id IS DISTINCT FROM NEW.owner_user_id OR OLD.name IS DISTINCT FROM NEW.name OR OLD.placement IS DISTINCT FROM NEW.placement OR OLD.storage IS DISTINCT FROM NEW.storage OR OLD.executor_id IS DISTINCT FROM NEW.executor_id OR OLD.executor_workspace_id IS DISTINCT FROM NEW.executor_workspace_id OR OLD.unregistered_at_ms IS DISTINCT FROM NEW.unregistered_at_ms)
EXECUTE FUNCTION ternilo_notify_cloud_live_change('workbench');

CREATE TRIGGER cloud_live_control_edge_sessions_insert_delete
AFTER INSERT OR DELETE ON control_edge_sessions
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_live_change('workbench');

CREATE TRIGGER cloud_live_control_edge_sessions_update
AFTER UPDATE ON control_edge_sessions
FOR EACH ROW WHEN (OLD.workspace_id IS DISTINCT FROM NEW.workspace_id OR OLD.owner_user_id IS DISTINCT FROM NEW.owner_user_id OR (OLD.metadata_json::jsonb - 'updated_at_ms') IS DISTINCT FROM (NEW.metadata_json::jsonb - 'updated_at_ms'))
EXECUTE FUNCTION ternilo_notify_cloud_live_change('workbench');

