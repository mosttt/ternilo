CREATE TRIGGER resource_live_workspace_projects_insert AFTER INSERT ON control_workspace_projects
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind,tenant_id) VALUES ('resources',NEW.tenant_id);
END;
CREATE TRIGGER resource_live_workspace_projects_update AFTER UPDATE ON control_workspace_projects
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind,tenant_id) VALUES ('resources',NEW.tenant_id);
END;
CREATE TRIGGER resource_live_workspace_projects_delete AFTER DELETE ON control_workspace_projects
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind,tenant_id) VALUES ('resources',OLD.tenant_id);
END;
