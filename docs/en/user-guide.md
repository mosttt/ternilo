# Workspaces and conversations

Start locally with [getting started](getting-started.md), or connect computers through [Server](remote-access.md). A workspace associates work with an actual directory and execution location. A Server project groups workspaces inside a space; creating a project does not create a filesystem directory.

## Start work

Choose the intended computer or managed location, open a directory, and create a conversation. Select its model, execution permissions and Agent preset before sending the first task. In Server, folder browsing happens on the selected computer. An offline computer can expose retained history but cannot accept file/configuration changes or execute work.

The input area distinguishes sending a new turn, queueing behind current work and injecting supported follow-up input. General settings control Enter behavior while busy. Inspect the queue to edit or remove pending submissions; stopping a running turn and removing a queued submission are different actions. A manual stop preserves completed operations and durable history.

## History and forks

Long histories load in bounded pages. Loading older events does not move the live-update cursor backwards. Completed turns can be displayed compactly while keeping reasoning and tool details expandable. Only reasoning text actually returned by the provider is shown.

A fork starts another conversation from a completed history prefix. It retains the captured preset; it does not make an already used conversation eligible to switch presets. Archive preserves history and is separate from deleting files. Renaming a workspace or conversation does not rename its project directory.

Use message attachments and explicit file references when they provide useful context. Generated artifacts can be previewed or downloaded in the file interface. See [files](files.md) for path boundaries and attachment behavior.

## Shared work

Permissions for viewing, submitting, stopping and configuring are separate. A shared task uses the conversation's configured execution permissions and selected model, with the actual submitter recorded. It does not create a separate OS account or copy the workspace. Draft text remains private to the author; shared events show accepted work. For independent simultaneous changes, use separate directories or Git worktrees.

See [collaboration](collaboration.md), [models](models.md), [settings](settings.md) and [project sharing](project-sharing.md). Back up computer data and project files as well as Server records when applicable.

## History loading

Streamed text fragments, tool events and status records count as underlying events, not visible messages. Histories below 10,000 events load completely. Larger histories initially load the latest 5,000 events and offer **Load earlier** in batches of up to 5,000. Each API request remains bounded to 1,000 events; the interface combines consecutive pages. Loading old events does not rewind the independent live cursor.
