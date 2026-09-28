-- PostgreSQL notifications are only a wake-up hint. The durable event journal
-- remains the canonical source read by Control under the tenant RLS scope.
CREATE FUNCTION ternilo_notify_cloud_session_event()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM pg_notify(
        'ternilo_cloud_session_events',
        json_build_object(
            'tenant_id', NEW.tenant_id,
            'session_id', NEW.session_id,
            'event_type', NEW.event->>'type'
        )::text
    );
    RETURN NEW;
END;
$$;

CREATE TRIGGER cloud_session_event_notify
AFTER INSERT ON cloud_session_events
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_cloud_session_event();

REVOKE ALL ON FUNCTION ternilo_notify_cloud_session_event() FROM PUBLIC;
