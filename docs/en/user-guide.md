# Workspaces and conversations

Start locally with [getting started](getting-started.md), or connect computers through [Server](remote-access.md). A workspace associates work with an actual directory and execution location. A Server project groups workspaces inside a space; creating a project does not create a filesystem directory.

## Start work

Choose the intended computer or managed location, open a directory, and create a conversation. Select its model, execution permissions and Agent preset before sending the first task. In Server, folder browsing happens on the selected computer. An offline computer can expose retained history but cannot accept file/configuration changes or execute work.

The input area distinguishes sending a new turn, queueing behind current work and injecting supported follow-up input. General settings control Enter behavior while busy. Inspect the queue to edit or remove pending submissions; stopping a running turn and removing a queued submission are different actions. A manual stop preserves completed operations and durable history.

Consecutive queued messages from the same authenticated account can enter one run while keeping separate messages, authors, attachments and references. Different authors form separate batches in FIFO order; later messages never jump over another author. Local input remains separate from platform accounts. Unattributed inputs and automated tasks without a provable common author run individually.

## History and forks

Long histories load in bounded pages. Loading older events does not move the live-update cursor backwards. Completed turns can be displayed compactly while keeping reasoning and tool details expandable. Only reasoning text actually returned by the provider is shown.

A fork starts another conversation from a completed history prefix. It retains the captured preset; it does not make an already used conversation eligible to switch presets. Archive preserves history and is separate from deleting files. Renaming a workspace or conversation does not rename its project directory.

Use message attachments and explicit file references when they provide useful context. Generated artifacts can be previewed or downloaded in the file interface. See [files](files.md) for path boundaries and attachment behavior.

## Shared work

Permissions for viewing, submitting, stopping and configuring are separate. A shared task uses the conversation's configured execution permissions and selected model, with the actual submitter recorded. It does not create a separate OS account or copy the workspace. Draft text remains private to the author; shared events show accepted work. For independent simultaneous changes, use separate directories or Git worktrees.

See [collaboration](collaboration.md), [models](models.md), [settings](settings.md) and [project sharing](project-sharing.md). Back up computer data and project files as well as Server records when applicable.

## History loading

The history API pages by raw events, not tokens or visible messages. Each request reads up to 1,000 events. Small histories below 10,000 events are filled automatically; larger histories normally start with 5,000 recent events. The first page appears immediately and starts Live catch-up while older pages merge in the background. A boundary inside a turn is extended to that turn's beginning, so one long reasoning round does not require a manual “Load earlier” action just because it contains many fragments.

Recent conversations use a bounded memory cache within the same account and space. Returning restores existing history and metadata immediately and resumes Live from the last accepted sequence. The cache is not persisted in browser storage and is cleared on logout or account/space changes; denied or unreadable sessions lose their cached content. Local, Desktop and Server use the same strategy. Streamed text is coalesced every 100 ms without discarding events; tool results and terminal state are processed immediately.

Local, Node and optional Cloud Worker “Stop and send all” acknowledge after saving queued inputs and restart intent and signalling cancellation. The execution host waits for the old run to release execution before sending the queue. A later explicit stop cancels restart intent. Acceptance is not proof of process exit. Cloud stores restart intent and the stop request in one transaction and retains them across Server restarts; the queue resumes after Worker or lease cleanup terminates the relevant run and releases its resources.

Workspace display names are unique within the same account and project. When another computer opens a same-named folder, the dialog adds the computer name, such as `Pictures (d)`. This does not rename the directory or remove the offline computer's workspace/history. A manually duplicated name produces an explicit validation message. The dialog loads project/computer lists when opened and refreshes them on request, without continuous polling.


## Computer groups and demand loading

The sidebar groups workspaces by computer by default. The grouping menu also supports workspace groups and a flat list. Computer collapse and ordering preferences are saved without changing execution bindings. Computer mode shows connectivity on the computer row, without repeating status dots on its workspaces. Workspace grouping retains each workspace's status indicator. Menus close when navigating to another page or using browser history.

“Only online computers” is available in computer mode. Initial state, Live workbench updates, search and archived lists request only visible resources on online computers. Server filters offline metadata before reading it rather than downloading everything and hiding it in the browser. Managed execution resources remain visible. Disabling the switch reloads all visible computers in the selected space. Switching replaces the subscription and discards stale responses; connectivity does not change authorization or delete history.
