# Project sharing inheritance

[简体中文](../zh-CN/project-sharing.md)

Project sharing applies common user/group policies to multiple workspaces only after each workspace owner opts in. Creating a project policy does not expose existing resources. Collaboration requires a team in multi-user mode; personal spaces remain private.

Space owners and administrators manage policies under Space Management → Projects → Project sharing rules. Policies grant view, submit, stop and session-configuration capabilities to current team members or permission groups. Team members may inspect policies. Project management alone does not grant access to other users' workspace content.

A workspace owner opens its sidebar More → Sharing dialog, inspects the project rules, and enables inheritance. Current and future project rules then cover that workspace, its sessions, archives and same-placement forks. New and existing workspaces start opted out. Viewer roles remain read-only regardless of broader policy grants.

| Identity | Project policies | Workspace opt-in | Resource content |
|---|---|---|---|
| Space owner/administrator | Can manage | Only for their own configurable workspaces | Still requires ownership or a grant |
| Workspace owner | Can inspect; management depends on space role | Can enable/disable with a writable space role | Existing owner access |
| Authorized member | Can inspect | Cannot change | Effective rights in opted-in workspaces |

Direct grants, workspace grants, groups and project policies combine, then space-role limits apply. Effective access shows the project and source. Opting out, removing a policy or leaving a group removes that source; independent grants remain. Forks never turn project policies into permanent personal grants: they continue inheriting current rules through their unchanged workspace, including when a user later rejoins a project group.

Inheritance does not grant deletion, direct-share management, opt-in changes, machine management, Provider credentials or extra model budget. Workspace owners retain the opt-in decision while administrators manage project rules. Sharing does not relocate directories, transfer resource ownership or create separate working copies. Ownership handoff and cross-project migration remain separate capabilities.

Once all view access is lost, workbench lists, Live content, files and archives become inaccessible. Accepted tasks retain the existing queue and stop semantics; revoking sharing alone does not cancel them. An actor with stop permission can stop them explicitly.

Control schema stays at `14`; the additive `project_sharing` component uses schema `1` for project user/group grants and workspace opt-in. Existing identities, owners and direct grants remain intact. PostgreSQL uses the migration owner to install this component and restricted tenant RLS at runtime, including an invoker-rights access view. Back up configuration and data before upgrading; older programs do not provide project inheritance. Executor protocol remains `44`.

See the [Server reference](../zh-CN/server-reference.md), [deployment guide](../zh-CN/deployment.md) and [collaboration guide](../zh-CN/collaboration.md) for related API, upgrade and filesystem behavior.
