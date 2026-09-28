-- Final schema. Business transitions run in shared Rust transactions.
CREATE TABLE cloud_runtime_control (
    singleton BIGINT PRIMARY KEY DEFAULT 1 CHECK (singleton = 1),
    claims_paused BIGINT NOT NULL DEFAULT 0 CHECK (claims_paused IN (0,1))
);

CREATE TABLE cloud_run_lineage (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    actor_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    root_run_id TEXT NOT NULL,
    parent_run_id TEXT,
    parent_lease_token BIGINT,
    parent_writer_fencing_token BIGINT,
    depth BIGINT NOT NULL CHECK (depth >= 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (tenant_id, run_id),
    FOREIGN KEY (tenant_id, root_run_id) REFERENCES cloud_run_lineage(tenant_id, run_id),
    FOREIGN KEY (tenant_id, parent_run_id) REFERENCES cloud_run_lineage(tenant_id, run_id),
    CHECK ((parent_run_id IS NULL AND parent_lease_token IS NULL
            AND parent_writer_fencing_token IS NULL AND depth = 0 AND root_run_id = run_id)
        OR (parent_run_id IS NOT NULL AND parent_run_id <> run_id
            AND parent_lease_token IS NOT NULL AND parent_writer_fencing_token IS NOT NULL
            AND parent_lease_token > 0 AND parent_writer_fencing_token > 0
            AND depth > 0 AND root_run_id <> run_id))
);

CREATE INDEX cloud_run_lineage_parent ON cloud_run_lineage(tenant_id, parent_run_id);

CREATE TABLE cloud_runs (
    input_provenance TEXT,
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    actor_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    authorization_session_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    spec TEXT NOT NULL,
    spec_digest BLOB NOT NULL CHECK (length(spec_digest) = 32),
    quota_reservation_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'queued', 'leased', 'running', 'cancel_requested',
        'succeeded', 'failed', 'cancelled', 'indeterminate'
    )),
    queue_wait_reason TEXT CHECK (queue_wait_reason IN ('workspace','capacity')),
    priority INTEGER NOT NULL DEFAULT 0 CHECK (priority BETWEEN -1000 AND 1000),
    attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    max_attempts INTEGER NOT NULL DEFAULT 1 CHECK (max_attempts BETWEEN 1 AND 10),
    available_at_ms BIGINT NOT NULL CHECK (available_at_ms >= 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    lease_owner TEXT,
    lease_token BIGINT NOT NULL DEFAULT 0 CHECK (lease_token >= 0),
    lease_expires_at_ms BIGINT,
    session_fencing_token BIGINT,
    started_at_ms BIGINT,
    finished_at_ms BIGINT,
    cancel_requested_at_ms BIGINT,
    outcome TEXT,
    error TEXT,
    PRIMARY KEY (tenant_id, run_id),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id),
    FOREIGN KEY (tenant_id, workspace_id, project_id)
        REFERENCES control_workspaces(tenant_id, workspace_id, project_id),
    FOREIGN KEY (tenant_id, quota_reservation_id)
        REFERENCES control_quota_reservations(tenant_id, reservation_id)
);

CREATE TABLE cloud_sessions (
    execution TEXT,
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    project_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT 'New session' CHECK (length(title) BETWEEN 1 AND 256),
    permissions TEXT NOT NULL DEFAULT 'workspace_write'
        CHECK (permissions IN ('read_only', 'workspace_write')),
    model_snapshot TEXT,
    reserved_model_tokens BIGINT NOT NULL DEFAULT 32768 CHECK (reserved_model_tokens > 0),
    agent_preset TEXT NOT NULL DEFAULT 'standard',
    profile_plugins TEXT NOT NULL DEFAULT '[]',
    mode TEXT NOT NULL DEFAULT 'execute' CHECK (mode IN ('execute', 'plan')),
    state TEXT NOT NULL CHECK (state IN (
        'idle', 'queued', 'running', 'succeeded', 'failed', 'cancelled', 'indeterminate'
    )),
    current_run_id TEXT,
    last_seq BIGINT NOT NULL DEFAULT -1 CHECK (last_seq >= -1),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    parent_session_id TEXT,
    archived_at_ms BIGINT CHECK (archived_at_ms >= created_at_ms),
    subagent_metadata TEXT,
    PRIMARY KEY (tenant_id, session_id),
    UNIQUE (tenant_id, session_id, user_id),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id),
    FOREIGN KEY (tenant_id, workspace_id, project_id)
        REFERENCES control_workspaces(tenant_id, workspace_id, project_id),
    FOREIGN KEY (tenant_id, current_run_id) REFERENCES cloud_runs(tenant_id, run_id),
    CHECK (subagent_metadata IS NULL OR (parent_session_id IS NOT NULL AND COALESCE(
        json_type(subagent_metadata)='object'
        AND json_type(subagent_metadata,'$.subagent_id')='text'
        AND length(json_extract(subagent_metadata,'$.subagent_id')) BETWEEN 1 AND 128
        AND json_extract(subagent_metadata,'$.subagent_id') NOT GLOB '*[' || char(1) || '-' || char(32) || char(127) || ']*'
        AND json_type(subagent_metadata,'$.provider')='text'
        AND length(trim(json_extract(subagent_metadata,'$.provider')))>0
        AND json_type(subagent_metadata,'$.transcript_kind')='text'
        AND json_extract(subagent_metadata,'$.transcript_kind') IN ('process_lifecycle','conversation')
        AND json_remove(subagent_metadata,'$.subagent_id','$.provider','$.transcript_kind')='{}',0)=1)),
    FOREIGN KEY (tenant_id, parent_session_id) REFERENCES cloud_sessions(tenant_id, session_id),
    FOREIGN KEY (tenant_id, parent_session_id, user_id) REFERENCES cloud_sessions(tenant_id, session_id, user_id)
);

CREATE TABLE cloud_workers (
    max_active_runs BIGINT NOT NULL DEFAULT 4 CHECK (max_active_runs > 0),
    max_resident_runs BIGINT NOT NULL DEFAULT 16 CHECK (max_resident_runs >= max_active_runs),
    worker_id TEXT PRIMARY KEY,
    instance_nonce TEXT NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    hello_json TEXT NOT NULL,
    registered_at_ms BIGINT NOT NULL CHECK (registered_at_ms >= 0),
    last_seen_at_ms BIGINT NOT NULL CHECK (last_seen_at_ms >= registered_at_ms),
    lease_expires_at_ms BIGINT NOT NULL CHECK (lease_expires_at_ms >= 0)
);

CREATE TABLE cloud_session_commands (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    actor_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    contributor_user_id TEXT REFERENCES control_users(user_id),
    session_id TEXT NOT NULL,
    command_id TEXT NOT NULL,
    command_seq BIGINT NOT NULL CHECK (command_seq >= 0),
    command_json TEXT NOT NULL,
    command_digest BLOB NOT NULL CHECK (length(command_digest) = 32),
    required_capability TEXT NOT NULL,
    required_catalog_revision TEXT,
    read_only BIGINT NOT NULL CHECK (read_only IN (0,1)),
    target_run_id TEXT,
    target_writer_fencing_token BIGINT,
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'inflight', 'completed', 'expired', 'indeterminate'
    )),
    lease_owner TEXT,
    worker_generation BIGINT,
    dispatch_lease_until_ms BIGINT,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    reply_json TEXT,
    issued_at_ms BIGINT NOT NULL CHECK (issued_at_ms >= 0),
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms > issued_at_ms),
    completed_at_ms BIGINT,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, command_id),
    UNIQUE (tenant_id, user_id, session_id, command_seq),
    FOREIGN KEY (tenant_id, session_id, user_id)
        REFERENCES cloud_sessions(tenant_id, session_id, user_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, target_run_id)
        REFERENCES cloud_runs(tenant_id, run_id),
    CHECK (
        (target_run_id IS NULL AND target_writer_fencing_token IS NULL)
        OR (target_run_id IS NOT NULL AND target_writer_fencing_token > 0)
    ),
    CHECK (read_only != 0 OR target_run_id IS NOT NULL),
    CHECK (
        (state = 'pending'
            AND lease_owner IS NULL
            AND worker_generation IS NULL
            AND dispatch_lease_until_ms IS NULL
            AND reply_json IS NULL
            AND completed_at_ms IS NULL)
        OR (state = 'inflight'
            AND lease_owner IS NOT NULL
            AND worker_generation > 0
            AND dispatch_lease_until_ms IS NOT NULL
            AND reply_json IS NULL
            AND completed_at_ms IS NULL)
        OR (state = 'completed'
            AND ((lease_owner IS NULL AND worker_generation IS NULL)
                OR (lease_owner IS NOT NULL AND worker_generation > 0))
            AND dispatch_lease_until_ms IS NULL
            AND reply_json IS NOT NULL
            AND completed_at_ms IS NOT NULL)
        OR (state IN ('expired', 'indeterminate')
            AND lease_owner IS NULL
            AND worker_generation IS NULL
            AND dispatch_lease_until_ms IS NULL
            AND reply_json IS NOT NULL
            AND completed_at_ms IS NOT NULL)
    )
);

CREATE TABLE cloud_session_inboxes (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    paused BIGINT NOT NULL DEFAULT 0 CHECK (paused IN (0,1)),
    error TEXT,
    next_position BIGINT NOT NULL DEFAULT 0 CHECK (next_position >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (tenant_id, user_id, session_id),
    FOREIGN KEY (tenant_id, session_id, user_id)
        REFERENCES cloud_sessions(tenant_id, session_id, user_id) ON DELETE CASCADE
);

CREATE TABLE cloud_session_submissions (
    input_provenance TEXT,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    actor_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    session_id TEXT NOT NULL,
    submission_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    requested_delivery TEXT NOT NULL CHECK (requested_delivery IN ('queue', 'steer')),
    content TEXT NOT NULL,
    submission_references TEXT NOT NULL DEFAULT '[]',
    attachments TEXT NOT NULL DEFAULT '[]',
    placement TEXT NOT NULL CHECK (placement IN ('queued', 'steering', 'running')),
    fifo_position BIGINT NOT NULL CHECK (fifo_position >= 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    steering_command_id TEXT,
    steering_target_run_id TEXT,
    steering_target_writer_fencing_token BIGINT,
    PRIMARY KEY (tenant_id, user_id, session_id, submission_id),
    UNIQUE (tenant_id, run_id),
    UNIQUE (tenant_id, user_id, session_id, fifo_position),
    FOREIGN KEY (tenant_id, user_id, session_id)
        REFERENCES cloud_session_inboxes(tenant_id, user_id, session_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, run_id)
        REFERENCES cloud_runs(tenant_id, run_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, steering_command_id) REFERENCES cloud_session_commands(tenant_id, command_id),
    FOREIGN KEY (tenant_id, steering_target_run_id) REFERENCES cloud_runs(tenant_id, run_id),
    CHECK ((steering_command_id IS NULL AND steering_target_run_id IS NULL AND steering_target_writer_fencing_token IS NULL)
        OR (steering_command_id IS NOT NULL AND steering_target_run_id IS NOT NULL AND steering_target_writer_fencing_token > 0))
);

-- Accepted uploads survive queue consumption, cancellation, and session archival.
CREATE TABLE cloud_session_uploads (
    tenant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    submission_id TEXT NOT NULL,
    attachment_index BIGINT NOT NULL CHECK (attachment_index >= 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    submitted_run_id TEXT NOT NULL,
    attachment TEXT NOT NULL,
    PRIMARY KEY (tenant_id, session_id, submission_id, attachment_index),
    FOREIGN KEY (tenant_id, session_id)
        REFERENCES cloud_sessions(tenant_id, session_id) ON DELETE CASCADE
);

CREATE TABLE cloud_session_writer_leases (
    tenant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    lease_owner TEXT NOT NULL,
    fencing_token BIGINT NOT NULL CHECK (fencing_token > 0),
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms >= 0),
    renewed_at_ms BIGINT NOT NULL CHECK (renewed_at_ms >= 0),
    PRIMARY KEY (tenant_id, session_id),
    FOREIGN KEY (tenant_id, session_id)
        REFERENCES cloud_sessions(tenant_id, session_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, run_id)
        REFERENCES cloud_runs(tenant_id, run_id) ON DELETE CASCADE
);

CREATE TABLE cloud_session_events (
    tenant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    seq BIGINT NOT NULL CHECK (seq >= 0),
    run_id TEXT NOT NULL,
    event TEXT NOT NULL,
    writer_fencing_token BIGINT NOT NULL CHECK (writer_fencing_token > 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (tenant_id, session_id, seq),
    FOREIGN KEY (tenant_id, session_id)
        REFERENCES cloud_sessions(tenant_id, session_id) ON DELETE CASCADE
);


CREATE TABLE cloud_session_questions (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    question_id TEXT NOT NULL CHECK (length(question_id) BETWEEN 1 AND 256),
    question TEXT NOT NULL,
    answer TEXT,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'answered')),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    answered_at_ms BIGINT,
    PRIMARY KEY (tenant_id, session_id, question_id),
    FOREIGN KEY (tenant_id, session_id, user_id)
        REFERENCES cloud_sessions(tenant_id, session_id, user_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, run_id)
        REFERENCES cloud_runs(tenant_id, run_id) ON DELETE CASCADE
);

CREATE TABLE cloud_attachment_objects (
    tenant_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    digest TEXT NOT NULL CHECK (length(digest) = 64 AND digest NOT GLOB '*[^0-9a-f]*'),
    content BLOB NOT NULL CHECK (length(content) BETWEEN 1 AND 67108864),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (tenant_id, workspace_id, digest),
    FOREIGN KEY (tenant_id, workspace_id)
        REFERENCES control_workspaces(tenant_id, workspace_id) ON DELETE CASCADE
);

CREATE TABLE cloud_agent_team_tasks (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    team_id TEXT NOT NULL CHECK (
        length(team_id) BETWEEN 1 AND 128
        AND team_id NOT GLOB '*[' || char(1) || '-' || char(32) || char(127) || ']*'
    ),
    task_id TEXT NOT NULL CHECK (
        length(task_id) BETWEEN 1 AND 128
        AND task_id NOT GLOB '*[' || char(1) || '-' || char(32) || char(127) || ']*'
    ),
    subject TEXT NOT NULL CHECK (length(TRIM(subject)) > 0),
    description TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'pending', 'in_progress', 'blocked', 'completed', 'cancelled'
    )),
    owner_member_id TEXT CHECK (
        owner_member_id IS NULL OR (
            length(owner_member_id) BETWEEN 1 AND 128
            AND owner_member_id NOT GLOB '*[' || char(1) || '-' || char(32) || char(127) || ']*'
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
        AND team_id NOT GLOB '*[' || char(1) || '-' || char(32) || char(127) || ']*'
    ),
    message_id TEXT NOT NULL CHECK (
        length(message_id) BETWEEN 1 AND 128
        AND message_id NOT GLOB '*[' || char(1) || '-' || char(32) || char(127) || ']*'
    ),
    from_member_id TEXT NOT NULL CHECK (
        length(from_member_id) BETWEEN 1 AND 128
        AND from_member_id NOT GLOB '*[' || char(1) || '-' || char(32) || char(127) || ']*'
    ),
    to_member_id TEXT NOT NULL CHECK (
        length(to_member_id) BETWEEN 1 AND 128
        AND to_member_id NOT GLOB '*[' || char(1) || '-' || char(32) || char(127) || ']*'
    ),
    content TEXT NOT NULL CHECK (length(TRIM(content)) > 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    read_at_ms BIGINT CHECK (read_at_ms IS NULL OR read_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, user_id, team_id, message_id)
);

CREATE TABLE cloud_session_telemetry (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    sharing_status TEXT NOT NULL CHECK (sharing_status IN (
        'disabled', 'feedback_only', 'full'
    )),
    handoff_seq BIGINT NOT NULL DEFAULT -1 CHECK (handoff_seq >= -1),
    export_seq BIGINT NOT NULL DEFAULT -1 CHECK (export_seq >= -1),
    last_error TEXT,
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (tenant_id, user_id, session_id),
    FOREIGN KEY (tenant_id, session_id, user_id)
        REFERENCES cloud_sessions(tenant_id, session_id, user_id) ON DELETE CASCADE
);

CREATE TABLE cloud_telemetry_outbox (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    occurrence_id TEXT NOT NULL,
    from_seq BIGINT NOT NULL CHECK (from_seq >= 0),
    to_seq BIGINT NOT NULL CHECK (to_seq >= from_seq),
    state TEXT NOT NULL CHECK (state IN ('pending', 'inflight', 'exported')),
    lease_owner TEXT,
    worker_generation BIGINT,
    export_lease_until_ms BIGINT,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_error TEXT,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    exported_at_ms BIGINT,
    PRIMARY KEY (tenant_id, occurrence_id),
    UNIQUE (tenant_id, user_id, session_id, from_seq, to_seq),
    FOREIGN KEY (tenant_id, user_id, session_id)
        REFERENCES cloud_session_telemetry(tenant_id, user_id, session_id) ON DELETE CASCADE,
    CHECK (
        (state = 'pending' AND lease_owner IS NULL AND worker_generation IS NULL
            AND export_lease_until_ms IS NULL AND exported_at_ms IS NULL)
        OR (state = 'inflight' AND lease_owner IS NOT NULL AND worker_generation > 0
            AND export_lease_until_ms IS NOT NULL AND exported_at_ms IS NULL)
        OR (state = 'exported' AND lease_owner IS NULL AND worker_generation IS NULL
            AND export_lease_until_ms IS NULL AND exported_at_ms IS NOT NULL)
    )
);

INSERT INTO cloud_runtime_control(singleton,claims_paused) VALUES (1,0);

CREATE INDEX cloud_runs_dispatch
ON cloud_runs (state, available_at_ms, priority DESC, created_at_ms)
WHERE state IN ('queued', 'leased');

CREATE INDEX cloud_runs_session
ON cloud_runs (tenant_id, session_id, created_at_ms);

CREATE INDEX cloud_runs_workspace
ON cloud_runs (tenant_id, workspace_id, created_at_ms);

CREATE UNIQUE INDEX cloud_session_submissions_running
ON cloud_session_submissions (tenant_id, user_id, session_id)
WHERE placement = 'running';

CREATE INDEX cloud_session_submissions_fifo
ON cloud_session_submissions (tenant_id, user_id, session_id, fifo_position);

CREATE INDEX cloud_sessions_active_owner
ON cloud_sessions (tenant_id, user_id, updated_at_ms DESC)
WHERE archived_at_ms IS NULL;

CREATE INDEX cloud_session_questions_pending
ON cloud_session_questions (tenant_id, user_id, session_id, created_at_ms)
WHERE state = 'pending';

CREATE INDEX cloud_session_commands_dispatch
ON cloud_session_commands (state, expires_at_ms, command_seq)
WHERE state IN ('pending', 'inflight');

CREATE INDEX cloud_session_commands_session
ON cloud_session_commands (tenant_id, user_id, session_id, command_seq);

CREATE INDEX cloud_session_submissions_steering_command
ON cloud_session_submissions (tenant_id, steering_command_id)
WHERE steering_command_id IS NOT NULL;

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

CREATE INDEX cloud_telemetry_outbox_dispatch
ON cloud_telemetry_outbox (state, created_at_ms, occurrence_id)
WHERE state IN ('pending', 'inflight');
