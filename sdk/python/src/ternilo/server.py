from __future__ import annotations

import json
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from collections.abc import Iterator, Mapping
from typing import Any

from .client import ProtocolError, RunResult, TerniloError, TransportClosedError


class ServerError(TerniloError):
    def __init__(self, code: str, message: str, status: int | None = None) -> None:
        super().__init__(message)
        self.code = code
        self.status = status


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_: Any, **__: Any) -> None:
        return None


class ServerClient:
    """HTTP and resumable Live access bound to one Server account and space."""

    def __init__(self, base_url: str, *, access_token: str, tenant_id: str,
                 request_timeout: float = 30.0) -> None:
        parsed = urllib.parse.urlsplit(base_url)
        if parsed.scheme not in ("http", "https") or not parsed.netloc or parsed.username or parsed.query or parsed.fragment:
            raise ValueError("base_url must be an HTTP(S) Server URL without credentials, query or fragment")
        if not access_token or not tenant_id or request_timeout <= 0:
            raise ValueError("access_token, tenant_id and a positive request_timeout are required")
        self._base = urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, parsed.path.rstrip('/'), '', ''))
        self._token, self._tenant = access_token, tenant_id
        self._timeout = request_timeout
        self._closed = threading.Event()
        self._sockets: set[Any] = set()
        self._lock = threading.Lock()
        self._http = urllib.request.build_opener(_NoRedirect())

    def __enter__(self) -> ServerClient:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def request(self, resource: str, *, method: str = "GET", body: Any = None,
                timeout: float | None = None) -> Any:
        if self._closed.is_set():
            raise TransportClosedError("Server client is closed")
        if not resource.startswith("/"):
            raise ValueError("resource must start with /")
        headers = {"Authorization": f"Bearer {self._token}", "X-Ternilo-Tenant": self._tenant}
        data = None if body is None else json.dumps(body).encode()
        if data is not None:
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(self._base + "/api/v1" + resource, data, headers, method=method)
        try:
            with self._http.open(request, timeout=self._timeout if timeout is None else timeout) as response:
                return None if response.status == 204 else json.load(response)
        except urllib.error.HTTPError as error:
            try:
                detail = json.load(error).get("error", {})
            except (ValueError, AttributeError):
                detail = {}
            finally:
                error.close()
            raise ServerError(detail.get("code", "http_error"), detail.get("message", f"Server HTTP {error.code}"), error.code) from error

    def state(self) -> dict[str, Any]:
        return self.request("/state")

    def create_session(self, workspace_id: str) -> dict[str, Any]:
        return self.request("/sessions", method="POST", body={"workspace_id": workspace_id})

    def history(self, session_id: str, *, before_seq: int | None = None, limit: int = 200) -> dict[str, Any]:
        query = {"limit": limit}
        if before_seq is not None:
            query["before_seq"] = before_seq
        return self.request(f"/sessions/{_id(session_id)}/history?{urllib.parse.urlencode(query)}")

    def submit(self, session_id: str, input: str, *, run_id: str | None = None) -> dict[str, Any]:
        return self.request(f"/sessions/{_id(session_id)}/queue", method="POST", body={
            "run_id": run_id or str(uuid.uuid4()), "content": {"kind": "prompt", "input": input},
        })

    def cancel(self, session_id: str, run_id: str) -> None:
        self.request(f"/sessions/{_id(session_id)}/turns/{_id(run_id)}", method="DELETE")

    def watch(self, session_id: str, *, after_seq: int | None = None,
              timeout: float | None = None, reconnect_timeout: float = 30.0) -> Iterator[dict[str, Any]]:
        """Yield event batches, including reset/complete markers, resuming after disconnects."""
        try:
            from websockets.exceptions import ConnectionClosed, InvalidMessage, InvalidStatus
            from websockets.sync.client import connect
        except ImportError as error:
            raise ImportError('Live access requires pip install "ternilo-sdk[remote]"') from error
        if reconnect_timeout <= 0 or (timeout is not None and timeout <= 0):
            raise ValueError("timeouts must be positive")
        deadline = None if timeout is None else time.monotonic() + timeout
        outage = time.monotonic()
        delay = 0.1
        url = self._base.replace("https:", "wss:", 1).replace("http:", "ws:", 1) + "/api/v1/live"
        while not self._closed.is_set():
            if time.monotonic() - outage >= reconnect_timeout:
                raise TimeoutError("Server Live reconnection timed out")
            ready = False
            received = False
            handshake_deadline = min(time.monotonic() + self._timeout, outage + reconnect_timeout,
                                     deadline if deadline is not None else float('inf'))
            try:
                with connect(url, open_timeout=_remaining(handshake_deadline, self._timeout),
                             close_timeout=1, max_size=16 * 1024 * 1024, max_queue=16) as socket:
                    with self._lock:
                        if self._closed.is_set():
                            return
                        self._sockets.add(socket)
                    try:
                        socket.send(json.dumps({"type": "hello", "protocol_version": 1,
                                                "bearer_token": self._token, "tenant_id": self._tenant}))
                        while not self._closed.is_set():
                            frame = json.loads(socket.recv(timeout=_remaining(deadline if ready else handshake_deadline, None)))
                            if frame.get("type") == "error":
                                raise ServerError(frame["code"], frame["message"])
                            if frame.get("type") == "ready":
                                if ready or frame.get("protocol_version") != 1:
                                    raise ProtocolError("unsupported Server Live handshake")
                                ready = True
                                subscription = {"type": "subscribe", "subscription_id": 1, "session_id": session_id,
                                                "metadata": dict.fromkeys(("inbox", "stats", "projection", "questions", "profile", "agent_team"), False)}
                                if after_seq is not None:
                                    subscription["after_seq"] = after_seq
                                socket.send(json.dumps(subscription))
                            elif frame.get("type") == "event_batch":
                                if not ready or frame.get("subscription_id") != 1 or frame.get("session_id") != session_id:
                                    raise ProtocolError("Server Live batch belongs to another subscription")
                                after_seq, frame = _batch(frame, after_seq)
                                received = True
                                delay = 0.1
                                yield frame
                    finally:
                        with self._lock:
                            self._sockets.discard(socket)
            except InvalidStatus as error:
                if error.response.status_code < 500:
                    raise ServerError("http_error", "Server Live handshake rejected", error.response.status_code) from error
            except InvalidMessage as error:
                # A restart can close TCP before the HTTP upgrade response is complete.
                if not isinstance(error.__cause__, EOFError):
                    raise
            except (ConnectionClosed, OSError):
                pass
            if self._closed.is_set():
                return
            # A healthy connection starts a new outage window only when it ends.
            if received:
                outage = time.monotonic()
            self._closed.wait(_remaining(deadline, min(delay, reconnect_timeout - (time.monotonic() - outage))))
            delay = min(delay * 2, 2.0)

    def run(self, session_id: str, input: str, *, timeout: float = 300.0) -> RunResult:
        page = self.history(session_id, limit=1)
        after = page["events"][-1]["seq"] if page["events"] else None
        run_id = str(uuid.uuid4())
        self.submit(session_id, input, run_id=run_id)
        events: list[dict[str, Any]] = []
        stream = self.watch(session_id, after_seq=after, timeout=timeout)
        try:
            for batch in stream:
                if batch["reset"]:
                    events.clear()
                for event in batch["events"]:
                    if event["run_id"] != run_id:
                        continue
                    events.append(event)
                    status = {"turn_finished": "idle", "turn_failed": "failed", "turn_cancelled": "cancelled"}.get(event["type"])
                    if status:
                        return RunResult(session_id, run_id, status, event.get("answer", ""), tuple(events), ())
        finally:
            stream.close()
        raise TransportClosedError("Server client closed before the run completed")

    def close(self) -> None:
        self._closed.set()
        with self._lock:
            sockets = tuple(self._sockets)
        for socket in sockets:
            socket.close()


def _id(value: str) -> str:
    return urllib.parse.quote(value, safe="")


def _remaining(deadline: float | None, maximum: float | None) -> float | None:
    remaining = None if deadline is None else deadline - time.monotonic()
    if remaining is not None and remaining <= 0 or maximum is not None and maximum <= 0:
        raise TimeoutError("Server operation timed out")
    return remaining if maximum is None else maximum if remaining is None else min(remaining, maximum)


def _batch(frame: Mapping[str, Any], cursor: int | None) -> tuple[int | None, dict[str, Any]]:
    events, next_seq, reset = frame.get("events"), frame.get("next_seq"), frame.get("reset")
    if not isinstance(events, list) or type(next_seq) is not int or next_seq < 0 or type(reset) is not bool or type(frame.get("complete")) is not bool:
        raise ProtocolError("invalid Server Live event batch")
    previous = None
    for event in events:
        seq = event.get("seq") if isinstance(event, dict) else None
        if type(seq) is not int or seq < 0 or previous is not None and seq <= previous or seq >= next_seq:
            raise ProtocolError("invalid Server Live event sequence")
        previous = seq
    if not reset and cursor is not None and next_seq < cursor + 1:
        raise ProtocolError("Server Live cursor moved backwards without a reset")
    return (next_seq - 1 if next_seq else None), {**frame, "events": [event for event in events if reset or cursor is None or event["seq"] > cursor]}
