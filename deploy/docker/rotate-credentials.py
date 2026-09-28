#!/usr/bin/env python3
"""Rotate Server-owned credentials and verify model changes through a real canary."""

import argparse
from importlib.machinery import SourceFileLoader
from importlib.util import module_from_spec, spec_from_loader
import json
import os
from pathlib import Path
import subprocess
import sys
from urllib.parse import quote


def deploy_module():
    loader = SourceFileLoader("ternilo_deploy", str(Path(__file__).resolve().with_name("ternilo-deploy")))
    module = module_from_spec(spec_from_loader(loader.name, loader))
    loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("master-key", "database-passwords", "model-key", "model-key-status", "model-key-commit", "model-key-rollback"))
    parser.add_argument("--directory", type=Path, default=Path(os.getenv("TERNILO_DEPLOY_DIR", ".")))
    parser.add_argument("--server-url", default=os.getenv("TERNILO_SERVER_PUBLIC_URL"))
    parser.add_argument("--docker", default=os.getenv("TERNILO_DOCKER", "docker"))
    parser.add_argument("--provider-id", default=os.getenv("TERNILO_MODEL_PROVIDER_ID"), help="Platform Provider to rotate; TERNILO_MODEL_PROVIDER_ID")
    parser.add_argument("--rotation-id", help="Exact pending rotation to commit or roll back after an interrupted operation")
    parser.add_argument("--next-key", help="Prefer TERNILO_NEXT_SECRET_MASTER_KEY or TERNILO_MODEL_API_KEY_NEXT.")
    arguments = sys.argv[1:]
    delimiter = arguments.index("--") if "--" in arguments else len(arguments)
    canary = arguments[delimiter + 1:]
    args = parser.parse_args(arguments[:delimiter])
    directory = args.directory.resolve()
    if (directory / ".ternilo-deployment").read_text().strip() != "server":
        parser.error("Credential rotation belongs to a Server deployment; Worker has no database or model key.")
    if args.operation == "model-key" and not canary:
        parser.error("Model-key rotation requires a real model canary command after --.")
    if args.operation != "model-key" and canary:
        parser.error("Only model-key rotation accepts a canary command.")
    if args.operation.startswith("model-key") and not args.provider_id:
        parser.error("Select the platform Provider with --provider-id or TERNILO_MODEL_PROVIDER_ID.")
    if args.operation in ("model-key-commit", "model-key-rollback") and not args.rotation_id:
        parser.error("Read model-key-status first, then pass its exact --rotation-id.")
    next_key = args.next_key or os.getenv("TERNILO_NEXT_SECRET_MASTER_KEY")
    candidate = args.next_key or os.getenv("TERNILO_MODEL_API_KEY_NEXT")
    if args.operation == "master-key" and not next_key:
        parser.error("Set TERNILO_NEXT_SECRET_MASTER_KEY for master-key rotation.")
    if args.operation == "model-key" and not candidate:
        parser.error("Set TERNILO_MODEL_API_KEY_NEXT for model-key rotation.")
    deployment = deploy_module()
    helper_path = Path(__file__).resolve().with_name("ternilo-deploy")
    compose = [args.docker, "compose", "--env-file", str(directory / ".env"), "-f", str(directory / "compose.server.yml")]
    origin = args.server_url or deployment.env_values(directory / ".env").get("TERNILO_SERVER_PUBLIC_URL")

    def helper(action, *, env=None):
        command = [str(helper_path), action, "--directory", str(directory), "--docker", args.docker]
        if origin:
            command += ["--server-url", origin]
        subprocess.run(command, env=dict(os.environ, **(env or {})), check=True)

    if args.operation.startswith("model-key"):
        resource = "/admin/models/providers/" + quote(args.provider_id, safe="") + "/key-rotation"
        drain_seconds = int(os.getenv("TERNILO_BACKUP_DRAIN_SECONDS", "300"))
        with deployment.server_api(origin) as api:
            if args.operation == "model-key-status":
                print(json.dumps(api(resource), indent=2))
                return 0
            if args.operation in ("model-key-commit", "model-key-rollback"):
                operation = args.operation.removeprefix("model-key-")
                deployment.set_execution_paused(api, True, drain_seconds)
                api(resource + "/" + quote(args.rotation_id, safe="") + "/" + operation, method="POST")
                deployment.set_execution_paused(api, False, drain_seconds)
                print(f"Provider key rotation {operation} completed; execution resumed.")
                return 0
            if api(resource)["rotation"] is not None:
                parser.error("This Provider has an unfinished rotation. Inspect model-key-status and explicitly commit or roll back its rotation ID.")
            deployment.set_execution_paused(api, True, drain_seconds)
            rotation = api(resource, {"api_key": candidate})
            rotation_path = resource + "/" + quote(rotation["rotation_id"], safe="")
            try:
                deployment.set_execution_paused(api, False, drain_seconds)
                subprocess.run(canary, check=True)
            except (OSError, ValueError, subprocess.CalledProcessError, KeyboardInterrupt):
                deployment.set_execution_paused(api, True, drain_seconds)
                api(rotation_path + "/rollback", method="POST")
                deployment.set_execution_paused(api, False, drain_seconds)
                print("Model canary failed. The previous encrypted Provider credential was restored; no plaintext key file was created.", file=sys.stderr)
                return 1
            api(rotation_path + "/commit", method="POST")
    else:
        helper("pause-execution")
        subprocess.run(compose + ["stop", "server"], check=True)
        if args.operation == "master-key":
            helper("rotate-key", env={"TERNILO_NEXT_SECRET_MASTER_KEY": next_key})
        else:
            helper("rotate-database-passwords")
        helper("up")
        helper("resume-execution")
    print(f"Server {args.operation} rotation completed; execution has resumed. Worker credentials were unchanged.")
    return 0


if __name__ == "__main__":
    os.umask(0o077)
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Credential rotation stopped: {error}. Keep private configuration for recovery; model-key-status reports any unfinished Provider rotation.", file=sys.stderr)
        sys.exit(1)
