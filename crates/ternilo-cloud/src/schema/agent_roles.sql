ALTER TABLE cloud_sessions ADD COLUMN subagent_role TEXT
CHECK (subagent_role IS NULL OR (
    subagent_metadata IS NOT NULL
    AND subagent_role IN ('investigator', 'implementer', 'reviewer')
));
