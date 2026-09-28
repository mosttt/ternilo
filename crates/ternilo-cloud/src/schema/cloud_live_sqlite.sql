-- Persistent wake-up metadata only. Canonical event bodies retain their existing scope.
CREATE TABLE cloud_live_changes (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    user_id TEXT,
    session_id TEXT,
    event_type TEXT
);

CREATE TRIGGER cloud_session_event_notify
AFTER INSERT ON cloud_session_events
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id, event_type)
    VALUES ('event', NEW.tenant_id,
        (SELECT user_id FROM cloud_sessions WHERE tenant_id = NEW.tenant_id AND session_id = NEW.session_id),
        NEW.session_id, json_extract(NEW.event, '$.type'));
END;

CREATE TRIGGER cloud_live_cloud_session_inboxes_insert
AFTER INSERT ON cloud_session_inboxes
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('inbox', NEW.tenant_id, NEW.user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_cloud_session_inboxes_update
AFTER UPDATE ON cloud_session_inboxes
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('inbox', NEW.tenant_id, NEW.user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_cloud_session_inboxes_delete
AFTER DELETE ON cloud_session_inboxes
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('inbox', OLD.tenant_id, OLD.user_id, OLD.session_id);
END;

CREATE TRIGGER cloud_live_cloud_session_submissions_insert
AFTER INSERT ON cloud_session_submissions
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('inbox', NEW.tenant_id, NEW.user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_cloud_session_submissions_update
AFTER UPDATE ON cloud_session_submissions
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('inbox', NEW.tenant_id, NEW.user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_cloud_session_submissions_delete
AFTER DELETE ON cloud_session_submissions
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('inbox', OLD.tenant_id, OLD.user_id, OLD.session_id);
END;

CREATE TRIGGER cloud_live_cloud_session_questions_insert
AFTER INSERT ON cloud_session_questions
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('questions', NEW.tenant_id, NEW.user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_cloud_session_questions_update
AFTER UPDATE ON cloud_session_questions
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('questions', NEW.tenant_id, NEW.user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_cloud_session_questions_delete
AFTER DELETE ON cloud_session_questions
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('questions', OLD.tenant_id, OLD.user_id, OLD.session_id);
END;

CREATE TRIGGER cloud_live_cloud_agent_team_tasks_insert
AFTER INSERT ON cloud_agent_team_tasks
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('agent_team', NEW.tenant_id, NEW.user_id, NULL);
END;

CREATE TRIGGER cloud_live_cloud_agent_team_tasks_update
AFTER UPDATE ON cloud_agent_team_tasks
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('agent_team', NEW.tenant_id, NEW.user_id, NULL);
END;

CREATE TRIGGER cloud_live_cloud_agent_team_tasks_delete
AFTER DELETE ON cloud_agent_team_tasks
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('agent_team', OLD.tenant_id, OLD.user_id, NULL);
END;

CREATE TRIGGER cloud_live_cloud_agent_team_task_dependencies_insert
AFTER INSERT ON cloud_agent_team_task_dependencies
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('agent_team', NEW.tenant_id, NEW.user_id, NULL);
END;

CREATE TRIGGER cloud_live_cloud_agent_team_task_dependencies_update
AFTER UPDATE ON cloud_agent_team_task_dependencies
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('agent_team', NEW.tenant_id, NEW.user_id, NULL);
END;

CREATE TRIGGER cloud_live_cloud_agent_team_task_dependencies_delete
AFTER DELETE ON cloud_agent_team_task_dependencies
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('agent_team', OLD.tenant_id, OLD.user_id, NULL);
END;

CREATE TRIGGER cloud_live_cloud_agent_team_messages_insert
AFTER INSERT ON cloud_agent_team_messages
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('agent_team', NEW.tenant_id, NEW.user_id, NULL);
END;

CREATE TRIGGER cloud_live_cloud_agent_team_messages_update
AFTER UPDATE ON cloud_agent_team_messages
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('agent_team', NEW.tenant_id, NEW.user_id, NULL);
END;

CREATE TRIGGER cloud_live_cloud_agent_team_messages_delete
AFTER DELETE ON cloud_agent_team_messages
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('agent_team', OLD.tenant_id, OLD.user_id, NULL);
END;

CREATE TRIGGER cloud_live_cloud_sessions_insert
AFTER INSERT ON cloud_sessions
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('session', NEW.tenant_id, NEW.user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_cloud_sessions_delete
AFTER DELETE ON cloud_sessions
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('session', OLD.tenant_id, OLD.user_id, OLD.session_id);
END;

CREATE TRIGGER cloud_live_cloud_sessions_update
AFTER UPDATE ON cloud_sessions
WHEN (OLD.workspace_id IS NOT NEW.workspace_id OR OLD.parent_session_id IS NOT NEW.parent_session_id OR OLD.subagent_metadata IS NOT NEW.subagent_metadata OR OLD.title IS NOT NEW.title OR OLD.archived_at_ms IS NOT NEW.archived_at_ms OR OLD.permissions IS NOT NEW.permissions OR OLD.model_snapshot IS NOT NEW.model_snapshot OR OLD.reserved_model_tokens IS NOT NEW.reserved_model_tokens OR OLD.agent_preset IS NOT NEW.agent_preset OR OLD.profile_plugins IS NOT NEW.profile_plugins OR OLD.mode IS NOT NEW.mode)
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('session', NEW.tenant_id, NEW.user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_session_activity_update
AFTER UPDATE ON cloud_sessions
WHEN (OLD.state IS NOT NEW.state OR OLD.execution IS NOT NEW.execution)
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('activity', NEW.tenant_id, NEW.user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_control_workspaces_insert
AFTER INSERT ON control_workspaces
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('workbench', NEW.tenant_id, NEW.owner_user_id, NULL);
END;

CREATE TRIGGER cloud_live_control_edge_sessions_insert
AFTER INSERT ON control_edge_sessions
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('workbench', NEW.tenant_id, NEW.owner_user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_control_workspaces_update
AFTER UPDATE ON control_workspaces
WHEN (OLD.project_id IS NOT NEW.project_id OR OLD.owner_user_id IS NOT NEW.owner_user_id OR OLD.name IS NOT NEW.name OR OLD.placement IS NOT NEW.placement OR OLD.storage IS NOT NEW.storage OR OLD.executor_id IS NOT NEW.executor_id OR OLD.executor_workspace_id IS NOT NEW.executor_workspace_id OR OLD.unregistered_at_ms IS NOT NEW.unregistered_at_ms)
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('workbench', NEW.tenant_id, NEW.owner_user_id, NULL);
END;

CREATE TRIGGER cloud_live_control_edge_sessions_update
AFTER UPDATE ON control_edge_sessions
WHEN (OLD.workspace_id IS NOT NEW.workspace_id OR OLD.owner_user_id IS NOT NEW.owner_user_id OR json_remove(OLD.metadata_json, '$.updated_at_ms') IS NOT json_remove(NEW.metadata_json, '$.updated_at_ms'))
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('workbench', NEW.tenant_id, NEW.owner_user_id, NEW.session_id);
END;

CREATE TRIGGER cloud_live_control_workspaces_delete
AFTER DELETE ON control_workspaces
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('workbench', OLD.tenant_id, OLD.owner_user_id, NULL);
END;

CREATE TRIGGER cloud_live_control_edge_sessions_delete
AFTER DELETE ON control_edge_sessions
BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id, user_id, session_id)
    VALUES ('workbench', OLD.tenant_id, OLD.owner_user_id, OLD.session_id);
END;
