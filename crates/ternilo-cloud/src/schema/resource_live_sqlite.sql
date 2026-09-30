-- Commit-scoped hints contain only the affected space, never shared content.

CREATE TRIGGER resource_live_control_resource_shares_insert
AFTER INSERT ON control_resource_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_resource_shares_update
AFTER UPDATE ON control_resource_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_resource_shares_delete
AFTER DELETE ON control_resource_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', OLD.tenant_id);
END;

CREATE TRIGGER resource_live_control_resource_group_shares_insert
AFTER INSERT ON control_resource_group_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_resource_group_shares_update
AFTER UPDATE ON control_resource_group_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_resource_group_shares_delete
AFTER DELETE ON control_resource_group_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', OLD.tenant_id);
END;

CREATE TRIGGER resource_live_control_resource_fork_group_sources_insert
AFTER INSERT ON control_resource_fork_group_sources
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_resource_fork_group_sources_update
AFTER UPDATE ON control_resource_fork_group_sources
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_resource_fork_group_sources_delete
AFTER DELETE ON control_resource_fork_group_sources
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', OLD.tenant_id);
END;

CREATE TRIGGER resource_live_control_permission_groups_insert
AFTER INSERT ON control_permission_groups
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_permission_groups_update
AFTER UPDATE ON control_permission_groups
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_permission_groups_delete
AFTER DELETE ON control_permission_groups
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', OLD.tenant_id);
END;

CREATE TRIGGER resource_live_control_permission_group_members_insert
AFTER INSERT ON control_permission_group_members
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_permission_group_members_update
AFTER UPDATE ON control_permission_group_members
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_permission_group_members_delete
AFTER DELETE ON control_permission_group_members
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', OLD.tenant_id);
END;

CREATE TRIGGER resource_live_control_project_user_shares_insert
AFTER INSERT ON control_project_user_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_project_user_shares_update
AFTER UPDATE ON control_project_user_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_project_user_shares_delete
AFTER DELETE ON control_project_user_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', OLD.tenant_id);
END;

CREATE TRIGGER resource_live_control_project_group_shares_insert
AFTER INSERT ON control_project_group_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_project_group_shares_update
AFTER UPDATE ON control_project_group_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_project_group_shares_delete
AFTER DELETE ON control_project_group_shares
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', OLD.tenant_id);
END;

CREATE TRIGGER resource_live_control_workspace_project_sharing_insert
AFTER INSERT ON control_workspace_project_sharing
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_workspace_project_sharing_update
AFTER UPDATE ON control_workspace_project_sharing
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_workspace_project_sharing_delete
AFTER DELETE ON control_workspace_project_sharing
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', OLD.tenant_id);
END;

CREATE TRIGGER resource_live_control_memberships_insert
AFTER INSERT ON control_memberships
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_memberships_update
AFTER UPDATE ON control_memberships
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', NEW.tenant_id);
END;

CREATE TRIGGER resource_live_control_memberships_delete
AFTER DELETE ON control_memberships
FOR EACH ROW BEGIN
    INSERT INTO cloud_live_changes (kind, tenant_id)
    VALUES ('resources', OLD.tenant_id);
END;
