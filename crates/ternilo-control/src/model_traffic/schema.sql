CREATE TABLE control_model_traffic_policy (
    singleton BIGINT PRIMARY KEY CHECK(singleton=1),
    revision BIGINT NOT NULL,
    policy_json TEXT NOT NULL
);
INSERT INTO control_model_traffic_policy VALUES(1,0,'{"platform":{},"account_default":{}}');
CREATE TABLE control_model_traffic_accounts (
    user_id TEXT PRIMARY KEY REFERENCES control_users(user_id),
    revision BIGINT NOT NULL,
    limits_json TEXT
);
CREATE INDEX control_model_traffic_recent ON control_model_requests(created_at_ms,actor_user_id);
CREATE INDEX control_model_traffic_pending ON control_model_requests(state,expires_at_ms,actor_user_id);
CREATE INDEX control_computer_traffic_recent ON control_computer_model_requests(created_at_ms,actor_user_id);
CREATE INDEX control_computer_traffic_pending ON control_computer_model_requests(state,updated_at_ms,actor_user_id);

CREATE INDEX control_model_traffic_actor_recent ON control_model_requests(actor_user_id,created_at_ms);
CREATE INDEX control_model_traffic_actor_pending ON control_model_requests(actor_user_id,state,expires_at_ms);
CREATE INDEX control_computer_traffic_actor_recent ON control_computer_model_requests(actor_user_id,created_at_ms);
CREATE INDEX control_computer_traffic_actor_pending ON control_computer_model_requests(actor_user_id,state,updated_at_ms);
