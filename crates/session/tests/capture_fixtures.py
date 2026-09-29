#!/usr/bin/env python3
"""capture_fixtures.py — regenerate tests/fixtures/ against a real evo-swarm.

Starts the installed `/usr/local/bin/evo-swarm serve` with the stub-provider
harness from `evo-agent/tests/swarm-serve-e2e.py` (temp EVO_HOME whose
init.lisp registers a stub provider, `--evo /usr/local/bin/evo-agent`, two
lanes, the scripted DELAY/SLOW/CALL model from tests/stub-messages.py), drives
it over HTTP exactly as the GUI will, and records the raw replies and raw SSE
bodies into tests/fixtures/.

Nothing here is a test: the tests load the fixtures.  This only records them.

  python3 crates/session/tests/capture_fixtures.py

The session is driven to quiescence before the consistent snapshot
(transcript + journal + lanes at cursor C, then /events?since=0 replayed up to
C), so the row reducer over that event log and the rows rebuilt from that
transcript describe the same session.  Shapes that need their own command
(/new, /compact, /eval) are captured after it, each in its own raw stream.

Log lines are UTC.
"""

import glob
import http.client
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime, timezone

EVO_AGENT = "/Users/bytedance/coding/evo-agent"
SWARM = "/usr/local/bin/evo-swarm"
EVO = "/usr/local/bin/evo-agent"
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "fixtures")
SECRET = "fixtures-stub-provider-secret"
LANES = 2

TODO_ITEMS = [{"text": "capture fixtures", "status": "in-progress"},
              {"text": "write unit tests", "status": "pending"},
              {"text": "report", "status": "done"}]
GOAL_TEXT = "fixture goal: show the goal segment FINISH"
# Lane 1 waits, then replaces its checklist; lane 2 reports and closes nothing.
LANE1_TASK = ('DELAY3 CALL todo {"items":[{"text":"lane step one","status":"in-progress"},'
              '{"text":"lane step two","status":"pending"}]}')
LANE2_TASK = ('CALL report {"done":"lane 2 finished the fixture work",'
              '"evidence":"lane2-transcript.json","next":"nothing",'
              '"blocked":"","requests":"none"}')


def log(message):
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    print(f"{stamp} {message}", flush=True)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Client:
    def __init__(self, port, token):
        self.port = port
        self.token = token

    def request(self, method, path, body=None, timeout=60):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=timeout)
        data = None
        headers = {"Authorization": f"Bearer {self.token}"}
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        conn.request(method, path, body=data, headers=headers)
        resp = conn.getresponse()
        raw = resp.read().decode(errors="replace")
        conn.close()
        try:
            return resp.status, json.loads(raw), raw
        except ValueError:
            return resp.status, raw, raw

    def get(self, path):
        return self.request("GET", path)

    def post(self, path, body=None, timeout=60):
        return self.request("POST", path, body if body is not None else {}, timeout=timeout)

    def state(self):
        return self.get("/state")[1]

    def lanes(self):
        return self.get("/lanes")[1]["lanes"]


def write_json(name, value):
    path = os.path.join(OUT, name)
    with open(path, "w") as f:
        json.dump(value, f, indent=2, ensure_ascii=False)
        f.write("\n")
    log(f"wrote {name} ({os.path.getsize(path)} bytes)")


def write_text(name, text):
    path = os.path.join(OUT, name)
    with open(path, "w") as f:
        f.write(text)
    log(f"wrote {name} ({os.path.getsize(path)} bytes)")


def read_raw_stream(port, token, path, stop=None, timeout=90, cap=4_000_000):
    """GET PATH and return the RAW response body text of an SSE stream, until
    STOP(text) says enough has arrived, the connection ends, or TIMEOUT."""
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    conn.request("GET", path, headers={"Authorization": f"Bearer {token}"})
    resp = conn.getresponse()
    if resp.status != 200:
        conn.close()
        raise RuntimeError(f"{path} -> HTTP {resp.status}")
    text = ""
    try:
        while True:
            piece = resp.fp.readline()
            if not piece:
                break
            text += piece.decode(errors="replace")
            if len(text) > cap or (stop is not None and stop(text)):
                break
    except (socket.timeout, TimeoutError, OSError):
        pass
    finally:
        conn.close()
    return text


def stop_after_event(name):
    """A stop predicate: true once NAME's event block is complete — its
    `event:` line seen and the blank line that ends the block arrived."""
    def stop(text):
        marker = f"\nevent: {name}\n"
        index = text.find(marker)
        return index >= 0 and "\n\n" in text[index + len(marker) - 1:]
    return stop


def stop_at_cursor(cursor):
    """A stop predicate: true once event CURSOR's block is complete — its
    `id:` line seen and the blank line that ends the block arrived."""
    def stop(text):
        marker = f"id: {cursor}\n"
        index = text.find(marker)
        return index >= 0 and "\n\n" in text[index + len(marker):]
    return stop


def parse_sse(text):
    """The SSE body as [{id, event, data}] — data parsed from JSON when it is
    JSON.  The raw text is the fixture of record; this is the same capture in
    the shape a Rust test loads directly."""
    events = []
    current = {"id": None, "event": None, "data": None}
    for line in text.split("\n"):
        line = line.rstrip("\r")
        if line == "":
            if current["event"]:
                events.append(current)
            current = {"id": None, "event": None, "data": None}
        elif line.startswith(":"):
            continue
        else:
            key, _, value = line.partition(": ")
            if key == "id":
                current["id"] = int(value)
            elif key == "data":
                try:
                    current["data"] = json.loads(value)
                except ValueError:
                    current["data"] = value
            else:
                current[key] = value
    return events


class Capture:
    """One background raw SSE capture, stopped by a predicate."""

    def __init__(self, port, token, path, stop, timeout=120):
        self.result = None
        self.error = None
        self.thread = threading.Thread(
            target=self._run, args=(port, token, path, stop, timeout), daemon=True)
        self.thread.start()

    def _run(self, port, token, path, stop, timeout):
        try:
            self.result = read_raw_stream(port, token, path, stop=stop, timeout=timeout)
        except Exception as e:                                   # noqa: BLE001
            self.error = repr(e)


class Stub:
    def __init__(self):
        self.port = free_port()
        self.proc = subprocess.Popen([sys.executable,
                                      os.path.join(EVO_AGENT, "tests", "stub-messages.py"),
                                      str(self.port)],
                                     stdout=subprocess.PIPE, text=True)
        line = self.proc.stdout.readline()
        assert line.startswith("stub listening"), line


class Swarm:
    """One `evo-swarm serve` process: its port, its token file, its log."""

    def __init__(self, home, proj, env, tag):
        self.port = free_port()
        self.token_file = os.path.join(os.path.dirname(home), f"token-{tag}")
        self.log = open(self.token_file + ".log", "w")
        self.proc = subprocess.Popen([SWARM, "serve", "--port", str(self.port),
                                      "--token-file", self.token_file,
                                      "--workers", str(LANES), "--evo", EVO],
                                     cwd=proj, env=env, stdin=subprocess.DEVNULL,
                                     stdout=self.log, stderr=subprocess.STDOUT,
                                     start_new_session=True)
        self.token = None
        self.client = None

    def wait_ready(self, timeout=90):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if os.path.exists(self.token_file) and os.path.getsize(self.token_file) > 0:
                self.token = open(self.token_file).read().strip()
                self.client = self.client or Client(self.port, self.token)
                if self.client.get("/health")[0] == 200:
                    return True
            if self.proc.poll() is not None:
                return False
            time.sleep(0.1)
        return False

    def kill(self):
        if self.proc.poll() is None:
            try:
                os.killpg(self.proc.pid, signal.SIGKILL)
            except OSError:
                self.proc.kill()
        self.log.close()


def write_init(home, text):
    with open(os.path.join(home, "init.lisp"), "w") as f:
        f.write(text)
    # the cache-stats extension, as the installed ~/.evo/extensions holds it,
    # so the fixture journal carries a real `cache-stats` custom entry.
    os.makedirs(os.path.join(home, "extensions"), exist_ok=True)
    shutil.copy(os.path.join(EVO_AGENT, "extensions", "340-cache-stats.lisp"),
                os.path.join(home, "extensions", "340-cache-stats.lisp"))


def wait_for(predicate, timeout=120, interval=0.2):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            value = predicate()
        except Exception:                                        # noqa: BLE001
            value = None
        if value:
            return value
        time.sleep(interval)
    return None


def main():
    os.makedirs(OUT, exist_ok=True)
    work = tempfile.mkdtemp(prefix="evo-fixtures-")
    home = os.path.join(work, "home")
    proj = os.path.join(work, "proj")
    os.makedirs(home)
    os.makedirs(proj)
    stub = Stub()
    log(f"stub provider on port {stub.port}; home {home}")

    write_init(home, f'(evo:register-provider :stub :base-url "http://127.0.0.1:{stub.port}" '
                     f':api-key "{SECRET}")\n'
                     '(evo:register-model "stub-a" :provider :stub :context-window 200000 '
                     ':max-output 8000 :effort t)\n'
                     '(evo:register-model "stub-b" :provider :stub :context-window 100000 '
                     ':max-output 8000 :effort t)\n'
                     '(evo:set-setting :model "stub-a")\n')

    env = dict(os.environ, EVO_HOME=home, EVO_BINARY=EVO, TERM="xterm-256color")
    for var in ("EVO_SERVE_TOKEN", "EVO_SUPERVISED_CHILD", "EVO_NO_SUPERVISOR",
                "EVO_SESSIONS_DIR", "EVO_SERVE_WATCH_PID", "ANTHROPIC_API_KEY"):
        env.pop(var, None)

    swarm = Swarm(home, proj, env, "main")
    port = swarm.port
    proc = swarm.proc
    try:
        if not swarm.wait_ready():
            raise RuntimeError(f"swarm did not come up (exit {proc.poll()})")
        c = swarm.client
        token = swarm.token
        write_json("health.json", c.get("/health")[1])
        log(f"swarm up on port {port}")

        # --- A: a fresh session, before anything ran --------------------------
        write_json("state.json", c.state())
        write_json("transcript-empty.json", c.get("/transcript")[1])
        write_json("registry.json", c.get("/registry")[1])
        write_json("journal-empty.json", c.get("/journal")[1])
        wait_for(lambda: c.lanes() if all(l["state"] == "idle" for l in c.lanes()) else None)
        write_json("lanes.json", c.get("/lanes")[1])

        # --- B: a goal, so /state carries one and a run can complete it -------
        write_json("reply-goal.json", c.post("/command", {"text": "/goal " + GOAL_TEXT})[1])
        write_json("state-with-goal.json", c.state())

        # --- C: the coordinator's own turn: one tool call, then text ----------
        write_json("reply-prompt.json",
                   c.post("/prompt", {"text": "CALL todo " + json.dumps({"items": TODO_ITEMS})})[1])
        wait_for(lambda: c.state() if c.state()["status"] == "idle" else None, 120)
        write_json("state-with-todos.json", c.state())

        # --- D: one lane does the checklist, the other reports ---------------
        lane1 = Capture(port, token, "/lanes/1/events",
                        stop=stop_after_event("settled"))
        time.sleep(0.3)
        write_json("reply-delegate-1.json",
                   c.post("/prompt", {"text": 'CALL delegate '
                                              + json.dumps({"lane": 1, "task": LANE1_TASK})})[1])
        working = wait_for(lambda: c.lanes() if any(l["n"] == 1 and l["state"] == "working"
                                                    for l in c.lanes()) else None, 60)
        write_json("lanes-lane1-working.json", c.get("/lanes")[1])
        log(f"lane 1 working on {working[0]['task']!r}")
        wait_for(lambda: c.lanes() if any(l["n"] == 1 and l["state"] == "idle"
                                          for l in c.lanes()) else None, 120)
        wait_for(lambda: c.state() if c.state()["status"] == "idle" else None, 120)
        lane1.thread.join(timeout=60)

        lane2 = Capture(port, token, "/lanes/2/events",
                        stop=stop_after_event("settled"))
        time.sleep(0.3)
        write_json("reply-delegate-2.json",
                   c.post("/prompt", {"text": 'CALL delegate '
                                              + json.dumps({"lane": 2, "task": LANE2_TASK})})[1])
        wait_for(lambda: c.lanes() if any(l["n"] == 2 and l["state"] == "idle"
                                          for l in c.lanes()) else None, 120)
        # the lane's report starts a coordinator run of its own: wait it out
        wait_for(lambda: c.state() if c.state()["status"] == "idle"
                 and all(l["state"] == "idle" for l in c.lanes()) else None, 180)
        lane2.thread.join(timeout=60)
        if lane1.error or lane2.error:
            raise RuntimeError(f"lane capture failed: {lane1.error} {lane2.error}")

        # --- E: quiescent = idle everywhere and the cursor standing still ----
        quiet_since = [None, None]

        def quiescent():
            state = c.state()
            if state["status"] != "idle" or any(l["state"] != "idle" for l in c.lanes()):
                quiet_since[0] = None
                return None
            if quiet_since[0] == state["cursor"]:
                if quiet_since[1] is None:
                    quiet_since[1] = time.time()
                elif time.time() - quiet_since[1] >= 3:
                    return state
            else:
                quiet_since[0] = state["cursor"]
                quiet_since[1] = None
            return None

        quiet = wait_for(quiescent, 120)
        if not quiet:
            raise RuntimeError("the session never went quiet")
        cursor = quiet["cursor"]
        log(f"quiescent at cursor {cursor}")

        # --- F: the consistent snapshot: transcript, journal, lanes ----------
        write_json("state-final.json", quiet)
        write_json("transcript.json", c.get("/transcript")[1])
        write_json("transcript-limit20.json", c.get("/transcript?limit=20")[1])
        write_json("journal.json", c.get("/journal")[1])
        write_json("journal-limit40.json", c.get("/journal?limit=40")[1])
        write_json("journal-limit4.json", c.get("/journal?limit=4")[1])
        write_json("lanes-final.json", c.get("/lanes")[1])
        write_json("lane1-transcript.json", c.get("/lanes/1/transcript")[1])
        write_json("lane2-transcript.json", c.get("/lanes/2/transcript")[1])

        # --- G: the whole event log up to C ----------------------------------
        raw_events = read_raw_stream(port, token, "/events?since=0",
                                    stop=stop_at_cursor(cursor), timeout=120)
        write_text("events-coordinator.sse", raw_events)
        write_json("events-coordinator.json", parse_sse(raw_events))
        write_text("lane1-events.sse", lane1.result)
        write_json("lane1-events.json", parse_sse(lane1.result))
        write_text("lane2-events.sse", lane2.result)
        write_json("lane2-events.json", parse_sse(lane2.result))

        # --- H: shapes that need their own command ---------------------------
        # /new -> session-switched
        switched = Capture(port, token, "/events",
                           stop=stop_after_event("session-switched"), timeout=60)
        time.sleep(0.3)
        write_json("reply-new.json", c.post("/command", {"text": "/new"})[1])
        switched.thread.join(timeout=60)
        if switched.result:
            write_text("events-session-switched.sse", switched.result)
            write_json("events-session-switched.json", parse_sse(switched.result))
        write_json("state-after-new.json", c.state())

        # /compact on the fresh session -> compaction-start/compaction-end
        compacted = Capture(port, token, "/events",
                            stop=stop_after_event("settled"), timeout=120)
        time.sleep(0.3)
        status, reply, _ = c.post("/command", {"text": "/compact"})
        write_json("reply-compact.json", reply)
        compacted.thread.join(timeout=120)
        if compacted.result:
            write_text("events-compact.sse", compacted.result)
            write_json("events-compact.json", parse_sse(compacted.result))

        # /eval queueing input off-thread -> user-input (best effort: the form
        # runs on the session thread, so it must not be relied on)
        try:
            off_thread = Capture(port, token, "/events",
                                 stop=stop_after_event("user-input"), timeout=45)
            time.sleep(0.3)
            status, reply, _ = c.post("/eval", {"form": '(progn (evo:steer "fixture user-input") '
                                                          '(evo:request-run :text "fixture user-input"))'},
                                      timeout=20)
            write_json("reply-eval-user-input.json", reply)
            off_thread.thread.join(timeout=45)
            if off_thread.result and "event: user-input" in off_thread.result:
                write_text("events-user-input.sse", off_thread.result)
                write_json("events-user-input.json", parse_sse(off_thread.result))
            wait_for(lambda: c.state() if c.state()["status"] == "idle" else None, 60)
        except Exception as e:                                   # noqa: BLE001
            log(f"user-input capture skipped: {e!r}")

        # --- I: a clean shutdown, and what it publishes ----------------------
        bye = Capture(port, token, "/events",
                      stop=stop_after_event("bye"), timeout=60)
        time.sleep(0.3)
        status, reply, _ = c.post("/shutdown")
        write_json("reply-shutdown.json", reply)
        log(f"shutdown -> {status}")
        bye.thread.join(timeout=60)
        if bye.result:
            write_text("events-shutdown.sse", bye.result)
            write_json("events-shutdown.json", parse_sse(bye.result))
        log(f"swarm exited {proc.wait(timeout=30)}")

        # --- J: the same model id under two providers ------------------------
        # /registry then holds stub-a twice, which is what makes the readout's
        # model segment print "stub-a (stub)" instead of the bare id.
        second_home = os.path.join(work, "home2")
        second_proj = os.path.join(work, "proj2")
        os.makedirs(second_home)
        os.makedirs(second_proj)
        write_init(second_home, f'(evo:register-provider :stub :base-url '
                                f'"http://127.0.0.1:{stub.port}" :api-key "{SECRET}")\n'
                                f'(evo:register-provider :stub2 :base-url '
                                f'"http://127.0.0.1:{stub.port}" :api-key "{SECRET}")\n'
                                '(evo:register-model "stub-a" :provider :stub :context-window 200000 '
                                ':max-output 8000 :effort t)\n'
                                '(evo:register-model "stub-a" :provider :stub2 :context-window 200000 '
                                ':max-output 8000 :effort t)\n'
                                '(evo:set-setting :model "stub-a")\n')
        second = Swarm(second_home, second_proj,
                       dict(env, EVO_HOME=second_home), "two-providers")
        try:
            if not second.wait_ready():
                raise RuntimeError("the two-provider swarm did not come up")
            sc = second.client
            write_json("state-two-providers.json", sc.state())
            write_json("registry-two-providers.json", sc.get("/registry")[1])
            write_json("reply-shutdown-two-providers.json", sc.post("/shutdown")[1])
            log(f"two-provider swarm exited {second.proc.wait(timeout=30)}")
        finally:
            second.kill()
    finally:
        swarm.kill()
        stub.proc.kill()

    names = sorted(os.path.basename(p) for p in glob.glob(os.path.join(OUT, "*")))
    print(f"\n{len(names)} fixtures in {OUT}:")
    for name in names:
        print(f"  {name}")
    shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
