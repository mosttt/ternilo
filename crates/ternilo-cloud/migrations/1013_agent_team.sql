ALTER TABLE cloud_sessions
    ADD COLUMN subagent_metadata JSONB,
    ADD CONSTRAINT cloud_sessions_subagent_metadata_shape CHECK (
        subagent_metadata IS NULL OR (
            parent_session_id IS NOT NULL
            AND jsonb_typeof(subagent_metadata) = 'object'
            AND subagent_metadata ?& ARRAY[
                'subagent_id', 'provider', 'transcript_kind'
            ]
            AND subagent_metadata - ARRAY[
                'subagent_id', 'provider', 'transcript_kind'
            ] = '{}'::jsonb
            AND jsonb_typeof(subagent_metadata->'subagent_id') = 'string'
            AND length(subagent_metadata->>'subagent_id') BETWEEN 1 AND 128
            AND subagent_metadata->>'subagent_id' !~ '[[:space:][:cntrl:]]'
            AND jsonb_typeof(subagent_metadata->'provider') = 'string'
            AND length(BTRIM(subagent_metadata->>'provider')) > 0
            AND jsonb_typeof(subagent_metadata->'transcript_kind') = 'string'
            AND subagent_metadata->>'transcript_kind' IN (
                'process_lifecycle', 'conversation'
            )
        )
    ),
    ADD CONSTRAINT cloud_sessions_parent_owner
        FOREIGN KEY (tenant_id, parent_session_id, user_id)
        REFERENCES cloud_sessions(tenant_id, session_id, user_id);

CREATE TABLE cloud_agent_team_tasks (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    team_id TEXT NOT NULL CHECK (
        length(team_id) BETWEEN 1 AND 128
        AND team_id !~ '[[:space:][:cntrl:]]'
    ),
    task_id TEXT NOT NULL CHECK (
        length(task_id) BETWEEN 1 AND 128
        AND task_id !~ '[[:space:][:cntrl:]]'
    ),
    subject TEXT NOT NULL CHECK (length(BTRIM(subject)) > 0),
    description TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'pending', 'in_progress', 'blocked', 'completed', 'cancelled'
    )),
    owner_member_id TEXT CHECK (
        owner_member_id IS NULL OR (
            length(owner_member_id) BETWEEN 1 AND 128
            AND owner_member_id !~ '[[:space:][:cntrl:]]'
        )
    ),
    revision BIGINT NOT NULL CHECK (revision > 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, user_id, team_id, task_id)
);

CREATE TABLE cloud_agent_team_task_dependencies (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    dependency_task_id TEXT NOT NULL,
    PRIMARY KEY (tenant_id, user_id, team_id, task_id, dependency_task_id),
    CHECK (task_id <> dependency_task_id),
    FOREIGN KEY (tenant_id, user_id, team_id, task_id)
        REFERENCES cloud_agent_team_tasks(tenant_id, user_id, team_id, task_id)
        ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, user_id, team_id, dependency_task_id)
        REFERENCES cloud_agent_team_tasks(tenant_id, user_id, team_id, task_id)
        ON DELETE RESTRICT
);

CREATE TABLE cloud_agent_team_messages (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    team_id TEXT NOT NULL CHECK (
        length(team_id) BETWEEN 1 AND 128
        AND team_id !~ '[[:space:][:cntrl:]]'
    ),
    message_id TEXT NOT NULL CHECK (
        length(message_id) BETWEEN 1 AND 128
        AND message_id !~ '[[:space:][:cntrl:]]'
    ),
    from_member_id TEXT NOT NULL CHECK (
        length(from_member_id) BETWEEN 1 AND 128
        AND from_member_id !~ '[[:space:][:cntrl:]]'
    ),
    to_member_id TEXT NOT NULL CHECK (
        length(to_member_id) BETWEEN 1 AND 128
        AND to_member_id !~ '[[:space:][:cntrl:]]'
    ),
    content TEXT NOT NULL CHECK (length(BTRIM(content)) > 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    read_at_ms BIGINT CHECK (read_at_ms IS NULL OR read_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, user_id, team_id, message_id)
);

CREATE INDEX cloud_agent_team_tasks_updated
ON cloud_agent_team_tasks (tenant_id, user_id, team_id, updated_at_ms, task_id);

CREATE INDEX cloud_agent_team_dependencies_target
ON cloud_agent_team_task_dependencies (
    tenant_id, user_id, team_id, dependency_task_id
);

CREATE INDEX cloud_agent_team_mailbox
ON cloud_agent_team_messages (
    tenant_id, user_id, team_id, to_member_id, created_at_ms, message_id
);

ALTER TABLE cloud_agent_team_tasks ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_agent_team_task_dependencies ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_agent_team_messages ENABLE ROW LEVEL SECURITY;

CREATE POLICY cloud_agent_team_tasks_owner_scope ON cloud_agent_team_tasks
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_agent_team_dependencies_owner_scope
ON cloud_agent_team_task_dependencies
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_agent_team_messages_owner_scope ON cloud_agent_team_messages
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);
