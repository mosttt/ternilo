CREATE TRIGGER resource_ownership_edge_deleted
AFTER DELETE ON control_edge_sessions
FOR EACH ROW BEGIN
    DELETE FROM control_resource_ownership
    WHERE tenant_id=OLD.tenant_id AND resource_kind='session' AND resource_id=OLD.session_id;
END;
