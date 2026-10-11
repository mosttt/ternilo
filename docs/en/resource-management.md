# Resource management handoff

[简体中文](../zh-CN/resource-management.md)

Workspace and session management can be transferred within a team to an active member with a writable team role. Personal spaces and projects do not support this operation. Project administration continues to follow team roles.

The current manager opens the resource's sharing panel, selects “Transfer management…”, searches for a recipient and confirms the handoff. By default, the former manager remains an editing collaborator who can view, submit, stop and configure sessions, without sharing, deleting or transferring management. The recipient can change or revoke this grant later. Clearing “Keep me as an editing collaborator” adds no access grant for the former manager; existing independent grants still apply.

Workspace handoff also changes sessions that inherit its management. Independently transferred sessions keep their managers. Transferring one session does not transfer its workspace or other sessions. New sessions inherit workspace management; when the manager forks an independently transferred session, that manager owns the new fork's management.

Handoff does not move directories, transfer computers or rewrite historical authors. Node credentials, storage bindings, session execution identity, past usage and configured execution sources remain independent. The recipient gains neither the original account's Provider credentials nor plugin installation rights; global execution configuration remains with the original account or computer owner. Existing grants remain. Handoff does not actively cancel accepted tasks, but further execution still checks current authorization. Removing the former manager's access can require renewed authorization for original-account models or tasks.

The Server checks the management revision. A stale request cannot overwrite a later handoff; refresh and confirm again. Rename a workspace before transferring it if the recipient already manages a workspace with that name in the same project. Deleting a session removes its independent management record, preventing a reused identifier from inheriting incorrect permissions.

All Server instances sharing a database must use the same version. Management changes produce transactional permission notifications so other instances re-read workbench and subscription permissions. This is separate from cross-Server Node command forwarding; Node routing follows the [architecture](architecture.md#remote-paths).

The API supports `GET .../sharing/ownership` for the manager and revision, `GET .../sharing/ownership/candidates` for paginated recipient search, and `PUT .../sharing/ownership` for handoff. Replace `...` with `/api/v1/workspaces/{workspace_id}` or `/api/v1/sessions/{session_id}`. The request fields are:

```json
{
  "owner_user_id": "recipient user_id",
  "expected_owner_user_id": "current manager user_id",
  "expected_revision": 0,
  "retain_previous_owner": true
}
```

`access.owner_user_id` identifies the current manager, `access.storage_user_id` identifies the original storage/execution identity, and `access.ownership_revision` records the management revision. `is_owner` and `is_execution_owner` report the current user's respective identities and must not be used interchangeably.

## Moving a workspace to a project

Server workspaces can move to another management project in the same personal or team space. The current manager with write access opens the workspace menu, selects “Move to project…”, chooses a destination and confirms. Project administrators do not gain permission to move another user's workspace.

The move retains workspace and session identifiers, history, files, computer bindings and managers. The execution project and task usage attribution remain unchanged. This operation organizes management projects; file relocation, execution identity changes and moving personal resources into a team are separate capabilities. A workspace with the same name and manager in the destination must be renamed first. Projects referenced by execution or management assignments cannot be deleted.

Project sharing inheritance is disabled by default: access inherited from the previous project ends, while direct workspace and session grants remain. Explicitly select “Apply the target project’s sharing rules” to apply its current and future rules; these can be inspected before confirmation. Changing the destination or sharing choice requires a new confirmation. Servers refresh workbenches and subscription permissions after the transaction commits. Instances sharing a database must use the same version before using project moves.

`GET /api/v1/workspaces/{workspace_id}/project` returns the management project, execution project, move revision and permission. Send a `PUT` to the same path:

```json
{
  "project_id": "target project_id",
  "expected_project_id": "current management project_id",
  "expected_revision": 0,
  "inherit_project_sharing": false
}
```

Stale revisions return 409, including requests from before a round trip between projects. Workspace `management_project_id` in `/state` identifies the management project, while the original `project_id` retains the execution project. The `/project` endpoint also returns an explicit `execution_project_id`.
