from __future__ import annotations

import json
import os
import queue
import subprocess
import threading
import time
from collections import deque
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping


class TerniloError(RuntimeError):
    pass


class ProtocolError(TerniloError):
    pass


class RemoteError(TerniloError):
    def __init__(self, code: int, message: str, data: Any = None) -> None:
        super().__init__(f"Ternilo RPC {code}: {message}")
        self.code = code
        self.data = data


class TransportClosedError(TerniloError):
    pass


@dataclass(frozen=True)
class RunResult:
    session_id: str
    run_id: str
    status: str
    answer: str
    events: tuple[dict[str, Any], ...]
    notifications: tuple[dict[str, Any], ...]


_CLOSED = object()


class HarnessClient:
    """Owns one `ternilo rpc` child and its versioned NDJSON connection."""

    def __init__(
        self,
        command: Iterable[str] = ("ternilo", "rpc"),
        *,
        cwd: str | os.PathLike[str] | None = None,
        env: Mapping[str, str] | None = None,
        request_timeout: float = 30.0,
        shutdown_timeout: float = 5.0,
    ) -> None:
        self._command = tuple(command)
        if not self._command:
            raise ValueError("command must not be empty")
        self._cwd = None if cwd is None else os.fspath(cwd)
        self._env = None if env is None else dict(env)
        self._request_timeout = request_timeout
        self._shutdown_timeout = shutdown_timeout
        self._process: subprocess.Popen[str] | None = None
        self._next_id = 0
        self._lock = threading.Lock()
        self._pending: dict[int, queue.Queue[Any]] = {}
        self._subscribers: dict[int, tuple[Callable[[dict[str, Any]], bool], queue.Queue[Any]]] = {}
        self._next_subscriber = 0
        self._stderr = deque[str](maxlen=200)
        self._reader: threading.Thread | None = None
        self._stderr_reader: threading.Thread | None = None
        self._closed = False

    def __enter__(self) -> HarnessClient:
        self.start()
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def start(self) -> dict[str, Any]:
        if self._closed:
            raise TransportClosedError("client is closed")
        if self._process is not None:
            return self.request("ping")
        self._process = subprocess.Popen(
            self._command,
            cwd=self._cwd,
            env=self._env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self._reader = threading.Thread(target=self._read_stdout, daemon=True)
        self._stderr_reader = threading.Thread(target=self._read_stderr, daemon=True)
        self._reader.start()
        self._stderr_reader.start()
        return self.request(
            "initialize",
            {
                "protocol_version": 1,
                "client": {"name": "ternilo-python", "version": "0.1.0"},
            },
        )

    def request(
        self,
        method: str,
        params: Mapping[str, Any] | None = None,
        *,
        timeout: float | None = None,
    ) -> dict[str, Any]:
        process = self._require_process()
        with self._lock:
            self._next_id += 1
            request_id = self._next_id
            reply: queue.Queue[Any] = queue.Queue(maxsize=1)
            self._pending[request_id] = reply
            frame: dict[str, Any] = {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
            }
            if params is not None:
                frame["params"] = dict(params)
            try:
                assert process.stdin is not None
                process.stdin.write(json.dumps(frame, separators=(",", ":")) + "\n")
                process.stdin.flush()
            except (BrokenPipeError, OSError) as error:
                self._pending.pop(request_id, None)
                raise self._closed_error("write failed") from error
        wait = self._request_timeout if timeout is None else timeout
        try:
            response = reply.get(timeout=wait)
        except queue.Empty as error:
            with self._lock:
                self._pending.pop(request_id, None)
            raise TimeoutError(f"Ternilo request {method!r} timed out after {wait}s") from error
        if response is _CLOSED:
            raise self._closed_error("runtime closed before responding")
        if not isinstance(response, dict):
            raise ProtocolError("response frame must be an object")
        if "error" in response:
            remote = response["error"]
            raise RemoteError(remote.get("code", -32000), remote.get("message", "error"), remote.get("data"))
        result = response.get("result")
        if not isinstance(result, dict):
            raise ProtocolError(f"response to {method!r} has no object result")
        return result

    def new_session(
        self,
        workspace_path: str | os.PathLike[str],
        *,
        session_id: str | None = None,
        agent_id: str | None = None,
        parent_session_id: str | None = None,
    ) -> dict[str, Any]:
        params: dict[str, Any] = {"workspace_path": str(Path(workspace_path).resolve())}
        if session_id is not None:
            params["session_id"] = session_id
        if agent_id is not None:
            params["agent_id"] = agent_id
        if parent_session_id is not None:
            params["parent_session_id"] = parent_session_id
        return self.request("session/new", params)

    def prompt(
        self,
        session_id: str,
        prompt: str,
        *,
        run_id: str | None = None,
        attachments: Iterable[Mapping[str, Any]] = (),
    ) -> dict[str, Any]:
        params: dict[str, Any] = {
            "session_id": session_id,
            "prompt": prompt,
            "attachments": [dict(value) for value in attachments],
        }
        if run_id is not None:
            params["run_id"] = run_id
        return self.request("session/prompt", params)

    def cancel(self, session_id: str, run_id: str) -> dict[str, Any]:
        return self.request("session/cancel", {"session_id": session_id, "run_id": run_id})

    def run(
        self,
        prompt: str,
        *,
        workspace_path: str | os.PathLike[str],
        session_id: str | None = None,
        attachments: Iterable[Mapping[str, Any]] = (),
        timeout: float = 300.0,
        on_notification: Callable[[dict[str, Any]], None] | None = None,
    ) -> RunResult:
        self.start()
        if session_id is None:
            created = self.new_session(workspace_path)
            session_id = created["identity"]["session_id"]
        subscription_id, notifications = self._subscribe(
            lambda frame: frame.get("params", {}).get("session_id") == session_id
        )
        try:
            receipt = self.prompt(session_id, prompt, attachments=attachments)
            run_id = receipt["run_id"]
            seen: list[dict[str, Any]] = []
            events: list[dict[str, Any]] = []
            deadline = time.monotonic() + timeout
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"Ternilo run {run_id!r} timed out after {timeout}s")
                try:
                    frame = notifications.get(timeout=remaining)
                except queue.Empty as error:
                    raise TimeoutError(f"Ternilo run {run_id!r} timed out after {timeout}s") from error
                if frame is _CLOSED:
                    raise self._closed_error("runtime closed during run")
                assert isinstance(frame, dict)
                if frame.get("params", {}).get("run_id") != run_id:
                    continue
                seen.append(frame)
                if on_notification is not None:
                    on_notification(frame)
                if frame.get("method") == "session.event":
                    event = frame["params"].get("event")
                    if isinstance(event, dict):
                        events.append(event)
                if frame.get("method") == "session.status" and frame["params"].get("status") in {
                    "idle",
                    "cancelled",
                    "failed",
                }:
                    status = frame["params"]["status"]
                    result = frame["params"].get("result", {})
                    answer = result.get("answer", "")
                    return RunResult(
                        session_id=session_id,
                        run_id=run_id,
                        status=status,
                        answer=answer,
                        events=tuple(events),
                        notifications=tuple(seen),
                    )
        finally:
            self._unsubscribe(subscription_id)

    def close(self) -> None:
        if self._closed:
            return
        process = self._process
        if process is None:
            self._closed = True
            return
        try:
            if process.poll() is None:
                self.request("shutdown", timeout=self._shutdown_timeout)
        except TerniloError:
            pass
        except TimeoutError:
            pass
        if process.stdin is not None:
            try:
                process.stdin.close()
            except OSError:
                pass
        try:
            process.wait(timeout=self._shutdown_timeout)
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=self._shutdown_timeout)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        if self._reader is not None:
            self._reader.join(timeout=self._shutdown_timeout)
        if self._stderr_reader is not None:
            self._stderr_reader.join(timeout=self._shutdown_timeout)
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                stream.close()
        self._closed = True
        self._fail_all()

    def _subscribe(
        self, predicate: Callable[[dict[str, Any]], bool]
    ) -> tuple[int, queue.Queue[Any]]:
        with self._lock:
            self._next_subscriber += 1
            subscription_id = self._next_subscriber
            values: queue.Queue[Any] = queue.Queue()
            self._subscribers[subscription_id] = (predicate, values)
            return subscription_id, values

    def _unsubscribe(self, subscription_id: int) -> None:
        with self._lock:
            self._subscribers.pop(subscription_id, None)

    def _read_stdout(self) -> None:
        process = self._process
        assert process is not None and process.stdout is not None
        try:
            for line in process.stdout:
                try:
                    frame = json.loads(line)
                except json.JSONDecodeError:
                    self._stderr.append(f"non-JSON stdout: {line.rstrip()}")
                    continue
                if not isinstance(frame, dict):
                    continue
                request_id = frame.get("id")
                if isinstance(request_id, int) and "method" not in frame:
                    with self._lock:
                        pending = self._pending.pop(request_id, None)
                    if pending is not None:
                        pending.put(frame)
                    continue
                if "method" in frame and "id" not in frame:
                    with self._lock:
                        subscribers = tuple(self._subscribers.values())
                    for predicate, values in subscribers:
                        if predicate(frame):
                            values.put(frame)
        finally:
            self._fail_all()

    def _read_stderr(self) -> None:
        process = self._process
        assert process is not None and process.stderr is not None
        for line in process.stderr:
            self._stderr.append(line.rstrip())

    def _fail_all(self) -> None:
        with self._lock:
            pending = tuple(self._pending.values())
            subscribers = tuple(values for _, values in self._subscribers.values())
            self._pending.clear()
        for values in (*pending, *subscribers):
            values.put(_CLOSED)

    def _require_process(self) -> subprocess.Popen[str]:
        process = self._process
        if process is None:
            raise TransportClosedError("client has not been started")
        if process.poll() is not None:
            raise self._closed_error("runtime is not running")
        return process

    def _closed_error(self, message: str) -> TransportClosedError:
        detail = "\n".join(self._stderr)
        return TransportClosedError(f"{message}{': ' + detail if detail else ''}")
