#!/usr/bin/env python3
"""A fake `evo-swarm serve`, speaking the protocol of CONTRACT.md §1/§5.

It exists so evo-gui's crates can drive a real client against the real wire
protocol — ready file, snapshot, SSE, `POST /ops` — without evo-agent, a model,
or a network. It interprets nothing: ops arrive over `POST /_emit` and go out to
every open stream exactly as they were given, and `/snapshot` answers whatever
`POST /_snapshot` last stored. The test decides what the bytes mean; this server
only moves them.

    fake_serve.py serve --port 0 --ready-file <path> [--watch-stdin] [...]

Control endpoints (`/_…`, not part of the protocol, never served by a real
server):

    POST /_snapshot  {"topics": {name: {"state": …, "items": [...], "has_more": b}}}
    POST /_emit      {"op": {…}}          assign a seq and publish to every stream
    POST /_reply     {"op": "input.send", "reply": {…}}   script an op's reply
    POST /_requests  {}                   the requests seen so far (and forget them)
    POST /_drop      {}                   close every open stream (a reconnect test)
    POST /_reset     {"reason": "restarted"}   publish a stream.reset to every stream
    POST /_epoch     {"epoch": "abcd"}    become a different process lifetime
    POST /_restart   {}                   re-exec: a new epoch, a new port, the
                                          same argv (a supervisor restart)
    POST /_forget    {}                   the next `since` is older than retention
"""

import json
import os
import re
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlparse

STARTED_AT = int(time.time() * 1000)


class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.epoch = uuid.uuid4().hex[:8]
        self.seq = 1
        # The topics a real swarm always has: the coordinator's own, and the
        # swarm record (state only, no items).
        self.topics = {
            "session": {"state": {"status": "idle"}, "items": []},
            "swarm": {"state": {"workers": 0, "status": {"busy": 0}, "lanes": []}},
        }
        self.replies = {}          # op name -> reply body (without rid/seq)
        self.replies_by_rid = {}   # rid -> the reply already given (idempotence)
        self.log = []              # (seq, payload), what a resume replays
        self.requests = []         # every request this server saw
        self.streams = []          # Stream, one per open SSE connection
        self.forget_next = False   # the next `since` is too old
        self.lanes = 0

    def next_seq(self):
        self.seq += 1
        return self.seq

    def broadcast(self, payload):
        for stream in list(self.streams):
            stream.push(payload)

    def close_streams(self):
        for stream in list(self.streams):
            stream.close()


class Stream:
    """One open SSE connection, written to from whichever thread has an op."""

    def __init__(self, handler):
        self.handler = handler
        self.lock = threading.Condition()
        self.queue = []
        self.closed = False

    def push(self, payload):
        with self.lock:
            if not self.closed:
                self.queue.append(payload)
                self.lock.notify_all()

    def close(self):
        with self.lock:
            self.closed = True
            self.lock.notify_all()

    def write(self, epoch, seq, payload, server):
        """Write frames until the stream is closed or the socket dies."""
        try:
            while True:
                with self.lock:
                    while not self.queue and not self.closed:
                        self.lock.wait(timeout=0.05)
                    if self.closed:
                        return
                    payload = self.queue.pop(0)
                seq = payload.get("seq", seq)
                frame = json.dumps(payload, separators=(",", ":"))
                self.handler.wfile.write(
                    f"id: {epoch}.{seq}\nevent: op\ndata: {frame}\n\n".encode()
                )
                self.handler.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, ValueError, OSError):
            return
        finally:
            with server.state.lock:
                if self in server.state.streams:
                    server.state.streams.remove(self)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    # --- plumbing ----------------------------------------------------------

    def log_message(self, *_args):
        pass

    def reply(self, status, body, content_type="application/json"):
        raw = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def body(self):
        length = int(self.headers.get("Content-Length") or 0)
        if not length:
            return {}
        raw = self.rfile.read(length)
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return {"_raw": raw.decode("utf-8", "replace")}

    def authorised(self):
        return self.headers.get("Authorization", "") == f"Bearer {TOKEN}"

    def record(self, path, body):
        with self.server.state.lock:
            self.server.state.requests.append(
                {
                    "method": self.command,
                    "path": path,
                    "token": self.headers.get("Authorization", ""),
                    "body": body,
                }
            )

    def do_GET(self):
        self.route(self.path, {})

    def do_POST(self):
        body = self.body()
        self.route(self.path, body)

    # --- routes ------------------------------------------------------------

    def route(self, path, body):
        url = urlparse(path)
        query = parse_qs(url.query)
        if url.path.startswith("/_") is False and not self.authorised():
            return self.reply(401, {"ok": False, "error": "bad token"})
        if not url.path.startswith("/_"):
            self.record(path, body)
        handler = {
            "/health": self.health,
            "/snapshot": self.snapshot,
            "/stream": self.stream,
            "/ops": self.ops,
            "/catalog": self.catalog,
            "/items": self.items,
            "/_snapshot": self.control_snapshot,
            "/_emit": self.control_emit,
            "/_reply": self.control_reply,
            "/_requests": self.control_requests,
            "/_drop": self.control_drop,
            "/_reset": self.control_reset,
            "/_epoch": self.control_epoch,
            "/_forget": self.control_forget,
            "/_restart": self.control_restart,
        }.get(url.path)
        if handler is None:
            if url.path.startswith("/items/"):
                return self.one_item(unquote(url.path[len("/items/") :]), query)
            if url.path.startswith("/media/"):
                return self.media(url.path)
            return self.reply(404, {"ok": False, "error": "no such route"})
        handler(query, body)

    def health(self, _query, _body):
        state = self.server.state
        self.reply(
            200,
            {
                "ok": True,
                "program": "evo-swarm",
                "version": "fake",
                "epoch": state.epoch,
                "pid": os.getpid(),
                "supervisor_pid": os.getpid(),
                "restarts": 0,
                "started_at": STARTED_AT,
                "session_loop_age_ms": 0,
            },
        )

    def expand(self, topics):
        names = []
        for name in topics:
            if name == "lane:*":
                names.extend(
                    key for key in self.server.state.topics if key.startswith("lane:")
                )
            elif name in self.server.state.topics:
                names.append(name)
        return names

    def snapshot(self, query, _body):
        state = self.server.state
        with state.lock:
            wanted = self.expand(query.get("topics", ["session"])[0].split(",") or [])
            limit = int((query.get("items") or ["200"])[0])
            topics = {}
            for name in wanted:
                stored = state.topics[name]
                items = stored.get("items", [])
                more = bool(stored.get("has_more")) or len(items) > limit
                topics[name] = {
                    "state": stored.get("state", {}),
                    "items": items[-limit:],
                    "has_more": more,
                }
            body = {"epoch": state.epoch, "seq": state.seq, "topics": topics}
        self.reply(200, body)

    def stream(self, query, _body):
        state = self.server.state
        topics = (query.get("topics") or ["session"])[0].split(",")
        since = (query.get("since") or [None])[0]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        stream = Stream(self)
        with state.lock:
            state.streams.append(stream)
            epoch, seq = state.epoch, state.seq
            forget = state.forget_next
            state.forget_next = False
            hello = {"op": "hello", "epoch": epoch, "seq": seq}
            if forget:
                hello["seq"] = 0
        try:
            self.wfile.write(
                f"id: {epoch}.{seq}\nevent: op\ndata: {json.dumps(hello, separators=(',', ':'))}\n\n".encode()
            )
            self.wfile.flush()
        except (BrokenPipeError, OSError):
            return
        if since and not forget:
            since_epoch, _, since_seq = since.rpartition(".")
            if since_epoch != epoch:
                stream.push({"op": "stream.reset", "reason": "restarted", "seq": seq})
            elif int(since_seq or 0) > seq:
                stream.push({"op": "stream.reset", "reason": "cursor_unknown", "seq": seq})
            else:
                # The retained op log: a resume replays what it missed, exactly as
                # the contract's 10-minute retention does.
                for missed in state.log:
                    if missed["seq"] > int(since_seq or 0):
                        stream.push(missed)
        if forget:
            stream.push({"op": "stream.reset", "reason": "cursor_too_old", "seq": seq})
        stream.write(epoch, seq, None, self.server)

    def ops(self, _query, body):
        state = self.server.state
        rid = body.get("rid")
        op = body.get("op")
        with state.lock:
            # A retried rid must not act twice (§5.5): answer from the store.
            if rid in state.replies_by_rid:
                return self.reply(200, state.replies_by_rid[rid])
            reply = dict(state.replies.get(op) or {"ok": True, "result": {}})
            seq = state.next_seq()
            reply["rid"] = rid
            reply["seq"] = reply.get("seq", seq)
            state.replies_by_rid[rid] = reply
        self.reply(200, reply)

    def catalog(self, _query, _body):
        state = self.server.state
        body = {
            "models": [
                {
                    "id": "fake-model",
                    "provider": "fake",
                    "name": "Fake",
                    "api": "fake",
                    "context_window": 1000,
                    "reasoning": True,
                    "images": False,
                    "ready": True,
                    "reason": None,
                }
            ],
            "providers": [{"name": "fake", "api": "fake", "has_key": True, "key_env": None}],
            "default_model": {"id": "fake-model", "provider": "fake"},
            "thinking_levels": ["off", "low", "medium", "high", "xhigh"],
            "languages": [{"code": "en", "name": "English"}],
            "ops": [{"name": "input.send", "args": {}, "precondition": "none"}],
            "commands": [],
            "skills": [],
            "tools": [],
            "warnings": [],
        }
        if state.lanes:
            body["lanes"] = {"models": [{"id": "fake-model", "provider": "fake", "ok": True, "reason": None}]}
        self.reply(200, body)

    def items(self, query, _body):
        state = self.server.state
        topic = (query.get("topic") or ["session"])[0]
        limit = int((query.get("limit") or ["100"])[0])
        before = (query.get("before") or [None])[0]
        with state.lock:
            stored = state.topics.get(topic, {})
            items = list(stored.get("items", []))
        if before is not None:
            index = next(
                (i for i, item in enumerate(items) if item.get("id") == before), len(items)
            )
            items = items[:index]
        self.reply(200, {"items": items[-limit:], "has_more": len(items) > limit})

    def one_item(self, item_id, _query):
        state = self.server.state
        with state.lock:
            for stored in state.topics.values():
                for item in stored.get("items", []):
                    if item.get("id") == item_id:
                        return self.reply(200, {"item": item})
        self.reply(404, {"ok": False, "error": "no such item"})

    def media(self, path):
        match = re.match(r"^/media/([^/]+)/(\d+)$", path)
        if not match:
            return self.reply(404, {"ok": False, "error": "no such media"})
        item_id = match.group(1)
        with self.server.state.lock:
            known = any(
                item.get("id") == item_id
                for stored in self.server.state.topics.values()
                for item in stored.get("items", [])
            )
        if not known:
            # Bytes belong to an item the server has; anything else is 404.
            return self.reply(404, {"ok": False, "error": "no such media"})
        self.reply(200, b"fake-image-bytes", content_type="image/png")

    # --- control -----------------------------------------------------------

    def control_snapshot(self, _query, body):
        state = self.server.state
        with state.lock:
            for name, stored in (body.get("topics") or {}).items():
                state.topics[name] = {
                    "state": stored.get("state", {}),
                    "items": stored.get("items", []),
                    "has_more": stored.get("has_more", False),
                }
            state.seq = body.get("seq", state.seq)
        self.reply(200, {"ok": True})

    def control_emit(self, _query, body):
        state = self.server.state
        with state.lock:
            seq = state.next_seq()
            epoch = state.epoch
            payload = dict(body.get("op") or {})
            payload["seq"] = payload.get("seq", seq)
            state.log.append(payload)
        state.broadcast(payload)
        self.reply(200, {"ok": True, "epoch": epoch, "seq": payload["seq"]})

    def control_reply(self, _query, body):
        with self.server.state.lock:
            self.server.state.replies[body.get("op")] = body.get("reply", {})
        self.reply(200, {"ok": True})

    def control_requests(self, _query, _body):
        with self.server.state.lock:
            seen = self.server.state.requests
            self.server.state.requests = []
        self.reply(200, {"requests": seen})

    def control_drop(self, _query, _body):
        self.server.state.close_streams()
        self.reply(200, {"ok": True})

    def control_reset(self, _query, body):
        state = self.server.state
        reason = body.get("reason", "restarted")
        with state.lock:
            payload = {"op": "stream.reset", "reason": reason, "seq": state.next_seq()}
        state.broadcast(payload)
        self.reply(200, {"ok": True})

    def control_epoch(self, _query, body):
        with self.server.state.lock:
            self.server.state.epoch = body.get("epoch") or uuid.uuid4().hex[:8]
            self.server.state.seq = 1
        self.reply(200, {"ok": True, "epoch": self.server.state.epoch})

    def control_restart(self, _query, _body):
        """Re-exec this server: a fresh epoch, a fresh port, the same argv.

        That is what a supervisor restart looks like from a client's side — the
        ready file is rewritten and the old connection dies — and it is why a
        client must re-read the ready file rather than trust the port it saw.
        """
        self.reply(200, {"ok": True})
        self.wfile.flush()
        threading.Thread(target=self._reexec, daemon=True).start()

    def _reexec(self):
        time.sleep(0.2)
        os.execv(sys.executable, [sys.executable] + sys.argv)

    def control_forget(self, _query, _body):
        with self.server.state.lock:
            self.server.state.forget_next = True
        self.reply(200, {"ok": True})


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, addr, state):
        super().__init__(addr, Handler)
        self.state = state


def write_ready(path, port):
    document = {
        "epoch": STATE.epoch,
        "pid": os.getpid(),
        "supervisor_pid": os.getpid(),
        "port": port,
        "url": f"http://127.0.0.1:{port}/",
        "token": TOKEN,
        "session": {"id": "fake-session", "path": "/tmp/fake-journal.sexp"},
        "program": "evo-swarm",
        "version": "fake",
        "restarts": 0,
    }
    temporary = f"{path}.tmp"
    with open(temporary, "w") as handle:
        json.dump(document, handle)
    os.chmod(temporary, 0o600)
    os.replace(temporary, path)
    return document


def watch_stdin(server, ready_file):
    """EOF on stdin means the tab is gone: stop, and take the ready file with it."""
    while True:
        chunk = sys.stdin.readline()
        if chunk == "":
            break
    try:
        os.remove(ready_file)
    except OSError:
        pass
    server.state.close_streams()
    os._exit(0)


STATE = State()
TOKEN = "0" * 64


def main(argv):
    if len(argv) < 2 or argv[1] != "serve":
        sys.stderr.write("fake_serve: expected `serve`, got %r\n" % (argv[1:2],))
        return 2
    ready_file = None
    port = 0
    watch = False
    index = 2
    while index < len(argv):
        flag = argv[index]
        value = argv[index + 1] if index + 1 < len(argv) else ""
        if flag == "--port":
            port = int(value)
            index += 2
        elif flag == "--ready-file":
            ready_file = value
            index += 2
        elif flag == "--watch-stdin":
            watch = True
            index += 1
        elif flag == "--workers":
            STATE.lanes = int(value)
            for n in range(1, STATE.lanes + 1):
                STATE.topics[f"lane:{n}"] = {"state": {"status": "idle"}, "items": []}
            STATE.topics["swarm"]["state"] = {
                "workers": STATE.lanes,
                "status": {"busy": 0},
                "lanes": [
                    {"n": n, "state": "idle", "reports": 0}
                    for n in range(1, STATE.lanes + 1)
                ],
            }
            index += 2
        else:
            index += 1
    if ready_file is None:
        sys.stderr.write("fake_serve: --ready-file is required\n")
        return 2
    server = Server(("127.0.0.1", port), STATE)
    bound = server.server_address[1]
    write_ready(ready_file, bound)
    sys.stderr.write(f"fake_serve: listening on http://127.0.0.1:{bound}/\n")
    sys.stderr.flush()
    if watch:
        threading.Thread(
            target=watch_stdin, args=(server, ready_file), daemon=True
        ).start()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        try:
            os.remove(ready_file)
        except OSError:
            pass
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
