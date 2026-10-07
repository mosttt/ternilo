# Managing registered computers

Server's **User Settings → My machines** manages your computers in the selected space. Space administrators can use the corresponding **Space Management → Computers** entry. See [remote access](remote-access.md) for enrollment.

Registered computers provide **Details and edit**, **Suspend/Resume access**, **Revoke computer**, and **Remove registration**. Revoked entries provide **Recover original computer access**. Details load when opened; removed entries load only after selecting **Show removed registrations**.

## Names, notes and details

Registration requires a computer name. Server generates a permanent, immutable computer ID for internal associations and routing, shown in details. Lists, workbench groups, breadcrumbs, directory selection, models and usage display the name.

Names are required and unique for the same account in the same space's registration list. They are case-sensitive and trimmed. Unused valid enrollment grants also reserve their names. Renaming preserves ID, credentials, directories and resource bindings. Removing a registration releases its name; recovering it with an occupied name requires choosing another name. Notes are stored on Server.

Details include registration ID, owner, project, connection and enrollment state, enrollment time, last contact, credential issue and last-use times, registered workspace count and associated session count. Previously connected computers also have expandable connection diagnostics. Last contact uses the latest heartbeat or authentication observed by Server. Unrecorded information is identified explicitly; hostnames and IP addresses are not inferred.

On mobile, swipe within the details area when its content exceeds the dialog height to view and edit the name, notes and connection diagnostics. Cancel and save actions remain at the bottom. When the available window height shrinks, the focused input scrolls into view.

Updates use the observed management version. If another page changed the same computer, refresh before retrying rather than overwriting newer information.

## Suspending and resuming access

Suspension rejects new Server connections and remote requests while retaining the original credential. A running Node can reconnect with that credential after access resumes, without enrolling again. The computer and network must still be available.

Suspension does not shut down the local Ternilo service or forcibly terminate all local tasks. Tasks using models through Server may be interrupted; inspect their results. To stop the local service normally, use `ternilo stop --data-dir <instance-directory>`.

## Revoking and removing registration

| Action | Credential | Registration list | Data |
|---|---|---|---|
| Suspend access | Retained; access is blocked until resumed | Retained and resumable | Retained |
| Revoke computer | Invalidated; original identity can be recovered | Retained with revoked status | Retained |
| Remove registration | Invalidated; recover through removed registrations | Hidden by default | Workspaces, session history and project files retained |

Revocation and removal require confirmation. Removing registration does not delete resource binding records. Synchronized offline history remains readable, so associated workspaces may remain in the sidebar. Unsynchronized content remains in the original computer's data directory.

## Recovering original computer access

Select **Recover original computer access** on a revoked computer. For a removed computer, first select **Show removed registrations**. Confirm its name and generate a recovery command. Recovery preserves the original ID, owner, project and resource references, issues a new credential, and leaves the old credential invalid. It does not replay historical tasks.

Stop the original instance gracefully before running the command on that computer. For a custom directory, append `--data-dir "<original-instance-directory>"`. Recovery retains the storage binding; a new empty directory cannot replace the original instance. Restore its backup first if the directory was lost. After successful connection verification, the new connection settings are saved privately for subsequent restarts.

**Register a new computer** creates an independent ID for a separate instance. Matching names never claim an existing identity. Multiple instances on one computer use separate directories, ports, IDs and credentials. Restarts, disconnections, computer IP changes and renaming do not enroll again. Credential rotation remains deferred; recovery handles revoked or removed computers only.
