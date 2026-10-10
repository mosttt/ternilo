# Accounts and collaboration

One Server account can register multiple computers, including several independent Ternilo instances on one physical host. Each Node has its own identity and credential. Local computer data and files stay on that computer; Server provides authenticated routing and retained metadata. See [remote access](remote-access.md).

Single-user mode admits only the instance owner. Multi-user mode supports other accounts and explicit sharing. Platform administration and team administration are separate: being a team administrator does not automatically grant access to every computer or private model.

## Accounts and sign-in

Native accounts register with a username, email and password. OIDC providers can use the upstream email and generate a platform username or ask the user to choose one; missing information and name conflicts require completion. New accounts still follow the platform's open, approval or invitation policy. Existing platform usernames do not change when the identity provider changes an external display name. See [authentication settings](server-authentication.md).

When account email delivery is enabled, users can verify their current email from account settings and use a verified native-account email to recover their password. An identity provider's email claim does not automatically verify the local account email. Password and local OIDC browser sessions share the same session management; revoking a local session does not end the identity provider's session. See [account security](account-recovery.md).

Revoked computer access can be restored with the original computer ID and data directory. Registering a separate instance requires a separate data directory. See [computer management](computers.md).

## Share a workspace or conversation

Use the resource's sharing action to select eligible users or permission groups. View, submit, stop and configure permissions are separate. Grant only the actions needed for the collaboration. Effective access is evaluated from current membership and grants; removing a grant or group member takes effect without copying stale rights into a session.

Project sharing inheritance is an explicit owner-controlled option. Grouping a workspace under a project alone does not grant access. See [project sharing](project-sharing.md) for the union of direct and inherited access and the limits of Node control.

Team workspaces and sessions also support [management handoff](resource-management.md) from their sharing panels. The recipient gains management while directories, computers, execution identity and historical authors remain unchanged. Original execution configuration and Provider credentials keep their separate permissions. Personal resources cannot be moved into a team through this operation.

Draft messages remain private to their author. Once accepted, tasks retain their actual submitter identity. Collaborators use the selected session's permissions, model binding and project files; sharing does not create a new OS sandbox, copy a directory or substitute the collaborator's private key. Independent concurrent edits should use separate worktrees or directories.

## Delegate a model to a shared session

For both connected computers and managed Worker sessions, a collaborator with configure permission can explicitly select their own account Provider or a platform model grant. Existing bindings retain the actual model provider; collaborators cannot browse or rebind unrelated private Providers. Platform grants used by other collaborators must allow resource sharing.

Workspace storage, execution identity and task limits belong to the resource's execution owner. Model access and platform budgets belong to the actual model provider, and requests retain the actual submitter. Calls, retries and active execution recheck the provider's configure permission and the submitter's submit permission. Revoking the model source stops its calls. Neither the Worker nor collaborators receive the upstream model key, and selecting a shared-session model does not change the owner's new-session default. See [model service](model-service.md).

## Offline and revoked access

Server can display the metadata and history it has already received while a computer is offline. New tasks, settings changes and file operations require the execution location to be available. Revoked access blocks future reads and commands even if an earlier browser still holds a visible copy. Account bans and mode changes are checked in addition to resource sharing.

A managed Worker is optional and registered through platform administration. It is distinct from connecting a personal computer and from the planned Work container product. See [platform management](platform-management.md), [Worker deployment](worker.md) and [security](security.md).
