#!/usr/bin/env python3
"""screens_model.py — the scripted model a screens capture runs.

The pictures are the app's own: the real window, the real `evo-swarm` /
`evo-agent` binaries, and the tool calls really executed. What is written for
the picture is only the model's *words*. This is the test stub the proofs use
(`evo-agent/tests/stub-messages.py`), imported and driven here, answering the
few natural prompts `crates/app/examples/screens.rs` types with prose a reader
would read instead of the stub's own shorthand (`ok: …`, `tool done`, and the
`slow0 slow1 …` filler).

`EVO_STUB_MESSAGES` points the fixture at this file, so it starts exactly where
the stock script would; everything below is upstream's: the HTTP surface, the
SSE envelope, `GET /_requests` and its record of the *original* prompts, and the
`CALL`/`DELAY` handling that makes the real binaries run a real tool.

What it decides, per request:

  "Review local image rendering and report back."
      the coordinator: `delegate {lane 1, task: <that same sentence>}`;
      a lane: three seconds of work, then `report {done, evidence, next}`
  "Show the transcript source files."
      `bash {ls crates/transcript/src}` — a real listing of a real folder
  "Explain how the image cache works."
      a real explanation, streamed a word at a time (the picture is taken in
      the middle of it)
  a tool result, a lane's report, anything else
      one line of prose: what the coordinator would say about it

Upstream is looked for exactly where the fixture looks: `EVO_AGENT_REPO` when it
is set, else the `evo-agent` beside this checkout. Nothing found is a loud
failure rather than a silent fall back to the stock script.

Usage: python3 screens_model.py PORT     (prints "stub listening PORT")
"""

import importlib.util
import json
import os
import pathlib
import re
import sys

# Importing the stub below compiles it, and a compiled module is cached beside its
# source by default — which would leave a `__pycache__` inside the evo-agent
# checkout. This capture reads that file; it does not write to it.
sys.dont_write_bytecode = True

STUB = "stub-messages.py"

# The prompts `screens.rs` types. They are matched as the stub matches its own
# markers — by what the text holds — so the same sentence reaches the coordinator
# and the lane it is delegated to.
DELEGATE = "Review local image rendering and report back."
SOURCES = "Show the transcript source files."
EXPLAIN = "Explain how the image cache works."

# What a lane reports back: the fields its card shows, as Markdown, naming the
# two sources the review is about. Nothing here claims a number the repository
# does not show, and nothing claims more than the code does.
REPORT = {
    "done": "**Local images in the message transcript**\n"
            "\n"
            "- Read and decoded off the UI thread.\n"
            "- Held in a bounded, memory-only cache keyed by path — another "
            "message naming the same file reuses its cached frame.\n"
            "- A file that is missing, too large or undecodable keeps the "
            "`[image: alt]` fallback.",
    "evidence": "- `crates/transcript/src/images.rs` — resolving a reference and "
                "keeping the frames\n"
                "- `crates/transcript/src/imgcheck.rs` — the decode, and the read "
                "and pixel limits",
    "next": "Ready for review. Unreadable files keep `[image: alt]`; remote "
            "references are not fetched automatically.",
}

# The answer to the prompt about the cache: the truth, streamed slowly enough to
# take a picture of it arriving. One word per delta upstream sends, so this wants
# to stay inside the sixty deltas that stream is made of.
EXPLANATION = (
    "The transcript resolves a message's picture against the tab's folder, reads "
    "and decodes it off the UI thread, and keeps the frame in a bounded cache "
    "keyed by its path — so another message naming the same file draws the frame "
    "it already has. Nothing is written to disk, and nothing is fetched over the "
    "network."
)

# What the stub's stock replies become, by stage: the coordinator's own words
# where the transcript would otherwise say `tool done` or echo the prompt.
LINES = {
    "delegate-done": "Lane 1 is on it — reviewing how a message's picture is read "
                     "and drawn, then it reports back.",
    "report": "Lane 1's report is in — frames are decoded off the UI thread and "
              "kept in a bounded cache.",
    "lane-idle": "Lane 1 is idle again.",
    "sources-done": "`images.rs` keeps the frames and `imgcheck.rs` does the "
                    "pixels — that is the whole of the path a local picture takes.",
    "lane-done": "Report sent.",
    "other": "Understood — carrying on.",
}

SLOW = re.compile(r"slow\d+ $")


def upstream():
    """The test stub this adapter drives: `EVO_AGENT_REPO`, else the sibling."""
    here = pathlib.Path(__file__).resolve()
    candidates = []
    if os.environ.get("EVO_AGENT_REPO"):
        candidates.append(
            pathlib.Path(os.environ["EVO_AGENT_REPO"]) / "tests" / STUB
        )
    # crates/app/examples/screens_model.py -> the checkout -> beside it, evo-agent.
    candidates.append(here.parents[4] / "evo-agent" / "tests" / STUB)
    for path in candidates:
        if path.is_file():
            return path
    sys.exit(
        "screens_model: no {} — set EVO_AGENT_REPO, or keep an evo-agent "
        "checkout beside this one (tried {})".format(
            STUB, ", ".join(str(path) for path in candidates)
        )
    )


def load(path):
    """The stub, as a module: imported from wherever it is, not installed."""
    spec = importlib.util.spec_from_file_location("stub_messages", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


stub = load(upstream())


def call(tool, args):
    """`CALL <tool> {json}` — upstream's own way to make the model call a tool."""
    return "CALL {} {}".format(tool, json.dumps(args))


def stage_of(text, has_result, record):
    """Which moment of the demo this request is: the newest user turn read the
    way upstream reads it (`text`), the whole request in `record`."""
    if record.get("summarizer"):
        return "summary"
    role = record.get("role", "")
    if role.startswith("lane"):
        # A lane's own two turns: the task it was given, and the result of the
        # report it sends.
        return "lane-done" if has_result else "lane-task"
    seen = " ".join(record.get("user_texts") or [])
    if has_result:
        # Which tool the result is from: the newest prompt the coordinator sent.
        return "sources-done" if SOURCES in seen else "delegate-done"
    if DELEGATE in text:
        return "delegate"
    if SOURCES in text:
        return "sources"
    if EXPLAIN in text:
        return "explain"
    if "[lane " in text and " report]" in text:
        return "report"
    if text.startswith("[lane "):
        return "lane-idle"
    return "other"


def plan(text, has_result, record):
    """What upstream is told to do, the prose its stock replies become, and the
    words a slow answer streams — one plan per request."""
    name = stage_of(text, has_result, record)
    scripts = {
        "delegate": lambda: call("delegate", {"lane": 1, "task": DELEGATE}),
        "lane-task": lambda: "DELAY3 " + call("report", REPORT),
        "sources": lambda: call("bash", {"command": "ls crates/transcript/src"}),
        "explain": lambda: "SLOW " + EXPLAIN,
    }
    script = scripts.get(name, lambda: text)()
    return script, LINES.get(name, LINES["other"]), (
        EXPLANATION.split() if name == "explain" else []
    )


class Handler(stub.Handler):
    """The stock handler, with the model's words decided here."""

    def respond(self, model, text, has_result, record):
        script, self.line, self.words = plan(text, has_result, record)
        self.said = 0
        super().respond(model, script, has_result, record)

    def sse(self, event, data):
        """Every delta on its way out: the stock text said as prose. The tool
        calls, and everything that is not text, go out untouched."""
        delta = data.get("delta") if isinstance(data, dict) else None
        if (
            event == "content_block_delta"
            and isinstance(delta, dict)
            and delta.get("type") == "text_delta"
        ):
            delta["text"] = self.in_words(delta.get("text", ""))
        super().sse(event, data)

    def in_words(self, text):
        if SLOW.match(text):
            return self.next_chunk()
        if self.line and (text == "tool done" or text.startswith("ok: ")):
            return self.line
        return text

    def next_chunk(self):
        """One word of the slow answer per delta upstream sends, so the prose
        arrives at the pace of the stream the picture is taken in."""
        if self.said < len(self.words):
            word = self.words[self.said]
            self.said += 1
            return word + " "
        return ""


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    server = stub.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.daemon_threads = True
    print("stub listening {}".format(server.server_address[1]), flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
