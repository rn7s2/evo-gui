# API gaps

Where the GUI wants something evo does not publish yet. Each entry is a thing the
design draws that the protocol cannot fill, so the app takes the simple fallback —
never an invented value — and says why here.

## A session's own name (`GET /sessions`, CONTRACT §2)

The index carries `title`, and it is the first text of the session's first **user-role**
entry — which for a session evo started itself is evo's own scaffolding: a goal's
continuation prompt ("You are idle but your goal is still active…"), a lane's brief
("FRESH SESSION: you are lane 2…"), a repo brief. A row named by it would be named by
evo rather than by the person who ran the session, so the history list names rows by
their folder instead (`crates/session/src/launcher.rs`'s `history_row`) and
`crates/store/src/history.rs` does not read `title` at all.

What the index would have to carry to change that: the first text a **person** typed
(the scaffolding of the harness's own first turn skipped), or a one-line summary evo
keeps per session.

## A model's own effort range (`GET /catalog`, CONTRACT §5.6)

The design's model rows read `200k ctx · vision · effort low–max`: the context window,
the modalities, and **the effort ladder that model accepts**. `/catalog` publishes
`context_window`, `reasoning:bool` and `images:bool` per model, and one global
`thinking_levels`, so the app can say *that* a model reasons but not which rungs it
takes — a model clamps what it is handed, and the global ladder is the session's, not
that model's.

`crates/session/src/launcher.rs`'s `model_detail` therefore prints `200k ctx · vision`
and stops: no effort range, and no `reasons` word either, which was this app's own
invention for the `reasoning` bool. The same line is what both model menus draw (the
New Swarm page and the composer's drawer, through `session::model_options`).

To close it: a per-model `effort: ["low", …, "max"]` (or `effort_levels`) in
`/catalog`'s `models[]`, beside `reasoning`. Then the detail line can be the design's
own, range and all.
