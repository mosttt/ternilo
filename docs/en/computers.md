# Managing registered computers

Server's **User Settings → My machines** manages your computers in the selected space. Space administrators can use the corresponding **Space Management → Computers** entry. See [remote access](remote-access.md) for enrollment.

Registered computers provide **Details and edit**, **Suspend/Resume access**, **Revoke computer**, and **Remove registration**. Refresh the list when needed; details are loaded only when opened.

## Names, notes and details

Display names appear in computer lists, workbench computer groups and execution-computer selectors. An empty name uses the registration ID. Editing a name does not change Node ID, credentials, directories, or workspace/session bindings. Notes are stored on Server and do not edit the computer's configuration.

Details include registration ID, owner, project, connection and enrollment state, enrollment time, last contact, credential issue and last-use times, registered workspace count and associated session count. Previously connected computers also have expandable connection diagnostics. Last contact uses the latest heartbeat or authentication observed by Server. Unrecorded information is identified explicitly; hostnames and IP addresses are not inferred.

Updates use the observed management version. If another page changed the same computer, refresh before retrying rather than overwriting newer information.

## Suspending and resuming access

Suspension rejects new Server connections and remote requests while retaining the original credential. A running Node can reconnect with that credential after access resumes, without enrolling again. The computer and network must still be available.

Suspension does not shut down the local Ternilo service or forcibly terminate all local tasks. Tasks using models through Server may be interrupted; inspect their results. To stop the local service normally, use `ternilo stop --data-dir <instance-directory>`.

## Revoking and removing registration

| Action | Credential | Registration list | Data |
|---|---|---|---|
| Suspend access | Retained; access is blocked until resumed | Retained and resumable | Retained |
| Revoke computer | Invalidated; enroll again to reconnect | Retained with revoked status | Retained |
| Remove registration | Invalidated; enroll again to reconnect | Hidden from the list | Workspaces, session history and project files retained |

Revocation and removal require confirmation. Removing registration does not delete resource binding records. Synchronized offline history remains readable, so associated workspaces may remain in the sidebar. Unsynchronized content remains in the original computer's data directory.

Using the same registration ID again requires explicit enrollment by an authorized account. It does not restore the old credential or replay historical tasks. Credential rotation is not available; revoke and enroll again when replacement is necessary.
