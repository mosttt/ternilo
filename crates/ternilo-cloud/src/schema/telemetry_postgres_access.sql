-- Candidate identities only; Rust owns all disclosure, retry, and lease decisions.
CREATE FUNCTION ternilo_cloud_telemetry_scopes(p_operation TEXT, p_worker TEXT, p_generation BIGINT, p_now_ms BIGINT)
RETURNS TABLE (tenant_id TEXT, user_id TEXT, session_id TEXT, occurrence_id TEXT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public, pg_temp AS $$
    SELECT item.tenant_id, item.user_id, item.session_id, item.occurrence_id FROM cloud_telemetry_outbox AS item
    WHERE (p_operation = 'claim' AND (item.state = 'pending' OR (item.state = 'inflight' AND item.export_lease_until_ms <= p_now_ms)))
       OR (p_operation = 'release' AND item.state = 'inflight' AND item.lease_owner = p_worker AND item.worker_generation = p_generation)
    ORDER BY item.created_at_ms, item.occurrence_id
$$;
