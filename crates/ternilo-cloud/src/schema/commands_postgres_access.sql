-- Scope discovery only. Rust rechecks authorization and leases inside each write transaction.
CREATE FUNCTION ternilo_cloud_command_scopes(p_operation TEXT, p_worker TEXT, p_generation BIGINT, p_now_ms BIGINT)
RETURNS TABLE (tenant_id TEXT, user_id TEXT, command_id TEXT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public, pg_temp AS $$
    SELECT command.tenant_id, command.user_id, command.command_id FROM cloud_session_commands AS command
    WHERE (p_operation = 'claim' AND command.expires_at_ms > p_now_ms AND (command.state = 'pending' OR (command.state = 'inflight' AND command.read_only = 1 AND command.dispatch_lease_until_ms <= p_now_ms)))
       OR (p_operation = 'release' AND command.state = 'inflight' AND command.lease_owner = p_worker AND command.worker_generation = p_generation)
       OR (p_operation = 'reap' AND ((command.state = 'pending' AND command.expires_at_ms <= p_now_ms) OR (command.state = 'inflight' AND (command.expires_at_ms <= p_now_ms OR command.dispatch_lease_until_ms <= p_now_ms))))
    ORDER BY command.issued_at_ms, command.command_seq, command.tenant_id, command.command_id
$$;
