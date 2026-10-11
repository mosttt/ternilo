CREATE TRIGGER resource_live_workspace_projects AFTER INSERT OR UPDATE OR DELETE ON control_workspace_projects
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();
