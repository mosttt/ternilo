# Server sign-in and human verification

The instance owner configures OIDC and Cloudflare Turnstile under **Platform administration → Instance settings → Sign-in and human verification**. Ordinary users and platform administrators cannot read or modify these settings. Saved changes apply to new requests without restart, use revision checks against concurrent edits, and override deployment-file defaults. Native password login remains available.

Single-user mode permits only the instance owner and rejects new account registration. The accounts page shows a mode notice; registration policy, approval settings and account invitations appear only in multi-user mode.

## OIDC

Set the public HTTPS URL and add named login providers. Each entry has a fixed generated ID, an independent enable switch, issuer, client ID, scopes including `openid`, and optional credentials. The sign-in page displays a named button for each available provider; account linking also lets users choose. Renaming a provider preserves existing sessions. Register the displayed callback URL with the identity provider. Browser sign-in uses authorization code with S256 PKCE and validates the signed ID Token, issuer, audience, timestamps, nonce and related claims. UserInfo must match the verified subject. An OAuth-only provider without the OIDC contract needs its own integration.

Use the URL actually opened in the browser. `0.0.0.0` and `::` are listening addresses and cannot be public or callback URLs. Configure an HTTPS reverse proxy with a valid certificate and use its external URL; changing the URL setting alone does not enable TLS. The browser URL, public Server URL and registered callback must share the same scheme, host and port.

PKCE requires browser Web Crypto. Pages without it, such as ordinary remote HTTP pages, show an HTTPS requirement before redirecting. A different callback origin also prevents sign-in because sessionStorage is origin-specific. An invalid saved public URL disables organization sign-in while retaining password access so the owner can correct it. Configured Turnstile checks remain enforced.

Public clients and `client_secret_basic` / `client_secret_post` are supported according to provider configuration. Server checks discovery and signing keys independently for each enabled provider. Unavailable entries are marked in settings and omitted from sign-in; other available providers and native login remain usable. Code exchange, sessions, refresh and MFA bind to the selected provider. Sessions from a provider are rejected while it is disabled or after it is removed, without deleting accounts or resources. The browser receives a short-lived Ternilo session, not an upstream token as its local identity. Upstream refresh credentials are encrypted on Server. Refresh is single-use and rejects replay, logged-out sessions and banned accounts.

Existing users explicitly link one OIDC identity to their account from account settings. Identity is issuer plus subject, not email or username; an equal email does not automatically merge accounts. Keep a working native owner password before changing issuer/client identity. The LINUX DO preset fills public endpoint/scope/authentication defaults but requires your own application credentials and a real production sign-in check.

## Registration policy

In multi-user mode, the accounts page offers open registration or administrator invitations, with an additional **OAuth2-only registration** condition. Enabling it requires an available OIDC provider. Open registration may also require approval; invited accounts do not require a second approval.

New users verify their identity with the chosen provider. A valid provider `preferred_username` or `username`, together with an email, completes registration automatically. A conflicting or unusable username requires an explicit choice. Provider email is used directly without an editable email field; the backend also ignores request parameters attempting to replace it. Users enter a contact email only when the provider did not supply one. Invitations, approval and Turnstile checks still follow instance policy. The backend rejects password registration and password-based invitation registration. Existing password sign-in, initial instance-owner setup and password changes remain available.

Account invitation tokens survive the OAuth redirect. Server consumes a valid, unexpired invitation and creates the account in one transaction; invalid or reused invitations cannot create accounts. Team invitations still require an existing account.

## Turnstile

Create a widget for the deployment hostname, enter its public Site Key and private Secret Key, then enable it. Server calls Siteverify and checks success, action and hostname. Invalid, expired or reused tokens fail; network errors do not bypass verification. Protected native login/registration forms request fresh challenges as needed. Node credentials and API keys do not use browser CAPTCHA.

CSP permits Cloudflare scripts/frames only while Turnstile is enabled. A reverse proxy must preserve compatible headers. Secrets are encrypted with the instance master key, are not echoed by management APIs and are not written into audit metadata.

## Recovery

Back up the database with its matching master key. PostgreSQL initialization uses the schema-owner connection before restricted runtime access. If an incorrect saved authentication configuration prevents login, use the trusted operator host:

```text
ternilo-server admin reset-authentication --config-dir /path/to
```

With the published Compose file, run this from the deployment directory:

```bash
docker compose -f compose.server.yml exec --user 10001:10001 server ternilo-server admin reset-authentication --config-dir /var/lib/ternilo
```

This clears web-saved OAuth/OIDC, Turnstile and mail settings, disables web-configured Turnstile and mail, and restores deployment OIDC defaults. It retains accounts, passwords, MFA, models, computers, workspaces and sessions, and writes audit history. Refresh the page, sign in with the existing password and configure these services again. There is no need to regenerate Server configuration or create another administrator.

Use the instance's actual configuration, database connection and master key. If the running process overrides the database address, first confirm that the maintenance command targets the same instance. Keep the original `config.json` and data volume: changing the master key can make encrypted credentials unreadable, and changing the database address selects a different database. Clearing browser cookies cannot repair settings stored in the Server database. For password recovery see [native account recovery](account-recovery.md). Browser session details and revocation are described in [settings](settings.md).

The owner-only authentication management API uses an `oidc_providers` array with fixed `id`, display `name`, `enabled` and independent credentials. `/auth/config` exposes available provider metadata and public authorization parameters. Code exchange, refresh and MFA requests must include the chosen `provider_id`. Empty secret inputs keep the matching saved secret; changing issuer/client ID requires a new one. Disabling a provider keeps its settings; removing it clears that entry.

Turnstile only accepts callbacks from the current widget after size or theme changes. Retired success, expiry and failure callbacks cannot change a newer widget token. Failures and expiry use explicit retry, with the provider error code displayed. Server-side validation remains required. Cloudflare `600*` errors describe a failed challenge and do not identify a unique application, browser or network cause. See the [official widget configuration](https://developers.cloudflare.com/turnstile/get-started/client-side-rendering/widget-configurations/) and [error codes](https://developers.cloudflare.com/turnstile/troubleshooting/client-side-errors/error-codes/).
