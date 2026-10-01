# Accounts and collaboration

One Server account can register multiple computers, including several independent Ternilo instances on one physical host. Each Node has its own identity and credential. Local computer data and files stay on that computer; Server provides authenticated routing and retained metadata. See [remote access](remote-access.md).

Single-user mode admits only the instance owner. Multi-user mode supports other accounts and explicit sharing. Platform administration and team administration are separate: being a team administrator does not automatically grant access to every computer or private model.

## Share a workspace or conversation

Use the resource's sharing action to select eligible users or permission groups. View, submit, stop and configure permissions are separate. Grant only the actions needed for the collaboration. Effective access is evaluated from current membership and grants; removing a grant or group member takes effect without copying stale rights into a session.

Project sharing inheritance is an explicit owner-controlled option. Grouping a workspace under a project alone does not grant access. See [project sharing](project-sharing.md) for the union of direct and inherited access and the limits of Node control.

Team workspaces and sessions also support [management handoff](resource-management.md) from their sharing panels. The recipient gains management while directories, computers, execution identity and historical authors remain unchanged. Original execution configuration and Provider credentials keep their separate permissions. Personal resources cannot be moved into a team through this operation.

Draft messages remain private to their author. Once accepted, tasks retain their actual submitter identity. Collaborators use the selected session's permissions, model binding and project files; sharing does not create a new OS sandbox, copy a directory or substitute the collaborator's private key. Independent concurrent edits should use separate worktrees or directories.

## Offline and revoked access

Server can display the metadata and history it has already received while a computer is offline. New tasks, settings changes and file operations require the execution location to be available. Revoked access blocks future reads and commands even if an earlier browser still holds a visible copy. Account bans and mode changes are checked in addition to resource sharing.

A managed Worker is optional and registered through platform administration. It is distinct from connecting a personal computer and from the planned Work container product. See [platform management](platform-management.md), [Worker deployment](worker.md) and [security](security.md).
