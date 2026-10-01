# Managed Worker deployment

`ternilo-worker` is the existing managed execution service. It is not the planned **Work** product with one managed computer per container. Work lifecycle, per-user container quotas and independent environment images are reserved design areas. A standalone client and Server do not require a Worker. Connect your own computer or VPS using [remote access](remote-access.md).

## Execution model

Server owns accounts, queue admission, model access and database state. Worker holds project files and executes jobs in separate child processes. Docker can host a Worker, but it does not create a Docker container per Harness. Linux execution uses Bubblewrap namespaces; the container mode drops the task process to UID/GID 10001 and clears capabilities.

Task environments have restricted mounts, isolated temporary storage and no network by default. Model calls are forwarded by the parent to Server; child processes do not receive model keys or database credentials. Jobs explicitly using the same workspace still share its files. A different preset does not create a different operating system or Python environment. The sandbox shares the host Linux kernel and is not VM isolation.

## Enable and register

Enable managed execution in Server with `--managed-execution-enabled` or `TERNILO_SERVER_MANAGED_EXECUTION_ENABLED=true`. In **Platform administration → Workers and execution**, register a Worker and save its one-time credential. Each Worker needs its own credential.

For native Linux deployment:

```bash
read -r -s -p "Worker token: " TERNILO_WORKER_TOKEN
export TERNILO_WORKER_TOKEN
ternilo-worker init --server-url https://ternilo.example.com --config-dir ./worker
unset TERNILO_WORKER_TOKEN
ternilo-worker serve --config-dir ./worker
```

For Docker, use `deploy/docker/ternilo-deploy init --component worker` with an explicitly available Worker image, Server URL and credential. The public Server image does not contain Worker, and current GitHub release publishing does not publish a Worker or Work image. The initialized volume retains Worker configuration and project files. Normal `down` preserves it; deleting the volume is destructive.

## Models and policy

Configure platform providers, keys, models and grants in Server's model service, or use an authorized account model. WorkerPolicy sets execution ceilings, tools, extensions and file quotas, not provider credentials. Shared execution uses the resource's selected model and original authorization; it does not silently switch to a collaborator's private key.

Worker must reach Server's normal URL even when deployed on the same host. It does not require a public inbound execution port. Diagnose health, credential status, policy and storage identity before restarting. Preserve workspace storage and Server records together during recovery; a new Worker with empty storage cannot reproduce the old files. See [deployment](deployment.md), [Server reference](server-reference.md) and [security](security.md).
