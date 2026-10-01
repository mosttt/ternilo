CREATE TRIGGER resource_live_ownership_insert AFTER INSERT ON control_resource_ownership
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind,tenant_id) VALUES ('resources',NEW.tenant_id);
END;
CREATE TRIGGER resource_live_ownership_update AFTER UPDATE ON control_resource_ownership
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind,tenant_id) VALUES ('resources',NEW.tenant_id);
END;
CREATE TRIGGER resource_live_ownership_delete AFTER DELETE ON control_resource_ownership
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind,tenant_id) VALUES ('resources',OLD.tenant_id);
END;
CREATE TRIGGER resource_ownership_cloud_deleted AFTER DELETE ON cloud_sessions
FOR EACH ROW BEGIN
    DELETE FROM control_resource_ownership
    WHERE tenant_id=OLD.tenant_id AND resource_kind='session' AND resource_id=OLD.session_id;
END;
