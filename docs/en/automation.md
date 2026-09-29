# Automation, ACP and SDKs

Use the same local application through the interface appropriate to the caller. A one-shot task can run with:

```text
ternilo run "Complete the task in this workspace" --json
```

`ternilo status --data-dir /path/to/data` and `ternilo stop --data-dir /path/to/data` inspect or stop the service belonging to that directory. Keep a separate data directory for independent automation instances. Local service discovery is authenticated; possession of a public session ID is not API authentication.

## JSON-RPC and ACP

```text
ternilo rpc --data-dir /path/to/automation-data
ternilo acp --data-dir /path/to/acp-data
```

The JSON-RPC interface is Ternilo's programmatic local protocol. ACP adapts the application to compatible Agent clients. Treat these as different protocols and use the matching client rather than mixing method names. Host permissions and execution ceilings still apply. Closing a transport or cancelling a request does not justify blindly replaying an external side effect.

The Python SDK sources are in `sdk/python/`; TypeScript sources are in `sdk/typescript/`. Their examples and tests show local process startup, requests, events, cancellation and cleanup. A binary package includes these sources for programmatic use but not a prebuilt language environment. Build/install the language package with its documented tooling.

## Remote Server access

Python and TypeScript also provide authenticated Server HTTP and Live clients. Use the Server URL, current login credential and selected tenant/resource scope. See [remote SDKs](remote-sdks.md) for exact imports and examples. Read retry, streaming cursor and abort behavior there before reconnecting production automation. Do not retry mutating requests solely because a network result was lost.

External ACP agents can be registered as subagent capabilities through plugin configuration. Their executable, arguments and access remain subject to the host policy. They do not automatically inherit arbitrary environment credentials. See [extensions](extensions.md), [Agent tools](agents.md) and [security](security.md).
