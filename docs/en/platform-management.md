# Platform administration

Server separates personal settings, team/resource administration and instance-wide administration. The sidebar's platform entry is for instance operations; a team owner/admin manages that team's members and projects. Neither role automatically grants every other account's computer or private credentials.

## Spaces, projects and groups

An account has a personal space and may join teams. A project groups workspaces within a space; it is not a directory. Creating or renaming a project does not create or rename files. Empty removable projects can be deleted; default/last projects and projects with associated resources/history are protected.

Permission groups simplify sharing to current team members. Resource view, submission, stop and configuration rights remain explicit. Group membership changes affect effective access without copying permanent membership into every resource. Optional project sharing inheritance must be enabled by the resource owner. See [project sharing](project-sharing.md).

## Accounts

The instance owner selects single-user or multi-user access and the registration policy. Registration, approval and invitation flows preserve a stable account identity. Matching email addresses do not automatically merge native and external identities; linking is explicit.

Banning or removing an account revokes access and invalidates credentials according to the operation. Unbanning does not revive previously revoked sessions or authorize replay of old commands. Do not treat account removal as proof that every external file, transferred resource or already-started side effect has been deleted. Ownership handoff and full resource cleanup remain separate administrative concerns.

## Instance services

The owner controls authentication and human-verification configuration. Platform model administration configures upstreams, published models, grants, groups and usage; it is separate from team collaboration groups. Managed execution is optional and registers Workers through its own entry.

Changing database type or deploying more Server replicas is not a user-mode toggle. Keep the supported single-Server deployment boundary and validate backups/restore through [deployment](deployment.md). See [model service](model-service.md), [authentication](server-authentication.md) and [Worker deployment](worker.md).
