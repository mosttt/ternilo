ALTER TABLE control_user_preferences
ADD COLUMN sidebar_ordering JSONB NOT NULL
DEFAULT '{"workspace_order":[],"session_order_by_account":{}}'::jsonb;
