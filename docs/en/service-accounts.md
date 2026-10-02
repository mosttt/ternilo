# Service accounts

Service accounts provide separate identities for automation. Open “Manage space → Service accounts” in the workbench, create a name and notes, then issue credentials. Names are unique within the space. Server generates an immutable `ter_sa_` ID; renaming preserves resource and history ownership.

Only space administrators and owners can manage service accounts. A service account cannot create accounts or issue credentials, and does not inherit the creator’s private workspaces, sessions or platform privileges. Each account and credential belongs to one explicit space.

## Credentials and permissions

An access credential starts with `ter_t_` and is shown only when issued. Lists show its name, scopes, expiry, last use and revocation status. Closing the details removes the full credential from the page. Choose a validity of 1–365 days; the default is 30 days.

| Scope | Allowed operations |
|---|---|
| `resource.read` | Read the current identity, projects, accessible workspaces, session lists, and authorized history, events, statistics, projections, queues, file content and run records |
| `run.execute` | Create workspaces and sessions, submit and stop tasks, and submit, edit, remove and steer queued messages |

Scopes cannot expand space membership or resource permissions. The service identity uses existing resource grants. Resources it creates retain the service account as owner and session identity. Read credentials cannot create tasks; execute credentials cannot access unauthorized private sessions. Managed execution must be enabled by the operator and meet existing executor and model requirements.

Credentials apply to HTTP APIs in the selected space. Include `X-Ternilo-Tenant`; a path containing the space ID can provide the scope instead, and a header supplied with such a path must match it. Service credentials cannot sign into the browser or access native login, OIDC, Live, account management, computer enrollment, Provider/user credential management or platform administration. Model API keys retain their separate authentication rules.

To read the current identity, replace the address, space ID and credential with actual values:

```http
GET /api/v1/me HTTP/1.1
Host: ternilo.example
Authorization: Bearer ter_t_<credential>
X-Ternilo-Tenant: ten_<space>
```

## Disabling and revocation

Disabling an account rejects future requests using any of its credentials. Re-enabling permits credentials that have neither expired nor been revoked. Revocation is permanent; issue a new credential when needed. Disabling and revoking preserve accounts, workspaces, history and original authorship.

Lists do not expose authentication digests. Last use reflects authenticated requests, normally updated once per minute. Changing spaces, closing details and leaving the page remove the one-time credential display. Audit entries include the operator, action, target and scopes without the full credential.

## Management APIs

These endpoints require a signed-in user with management permissions in `{tenant_id}`:

| Method and path | Behavior |
|---|---|
| `GET /api/v1/tenants/{tenant_id}/service-accounts` | List accounts |
| `POST /api/v1/tenants/{tenant_id}/service-accounts` | Create with `name` and optional `notes` |
| `PATCH /api/v1/tenants/{tenant_id}/service-accounts/{service_account_id}` | Update `name`, `notes`, `enabled` and `expected_revision` |
| `GET /api/v1/tenants/{tenant_id}/service-accounts/{service_account_id}/credentials` | List credential metadata |
| `POST /api/v1/tenants/{tenant_id}/service-accounts/{service_account_id}/credentials` | Issue with `name`, `scopes`, `expires_at_ms`; return one-time `access_token` and `credential` |
| `DELETE /api/v1/tenants/{tenant_id}/service-accounts/{service_account_id}/credentials/{credential_id}` | Revoke one credential |

SQLite and PostgreSQL use the same data component. PostgreSQL restricts table access to the current tenant scope. The configured Server database persists accounts and credentials, so database backups include them.
