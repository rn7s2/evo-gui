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

## A scroll area that decides its own height (`gpui-component`'s `Scrollable`)

`ScrollableElement::overflow_y_scrollbar()` wraps the element: a new root becomes the box a
reader sees (it copies the element's size, min/max and flex fields) and the element becomes
the *content* inside it, with the scroll area (`size_full`) between the two. That wants an
ancestor that already has a height — the area's `height: 100%` resolves against its parent,
and against an auto-height one it comes out zero, taking the content with it. Measured on the
history list: the box rendered 2px tall, its border and nothing else, rows clipped away.

So the two boxes the design sizes from their own content — the history list (hugs its rows
until the page runs out) and the boot log tail (hugs the log up to its 320px cap) — take the
other road: the element keeps its own overflow, and the bar is added beside it
(`ScrollableElement::vertical_scrollbar(&handle)`, which preserves the element as the scroll
area) inside a frame that holds the height. Two things come with that road:

- the handle has to outlive the frame — the history list's lives on `EmptyTabState`, the boot
  log's in `window.use_keyed_state`, keyed per tab — because a handle made per render reads
  back as offset zero on every frame, and the box cannot be scrolled at all;
- the bar has to be a *sibling* of the scroll box, not a child of it: a `ScrollbarLayer`
  inside the scroller is painted through that scroller's own translation, so a list scrolled
  to the bottom drew its thumb at the top, at the wrong length, in the resting colour.

To close it: a `Scrollable` that lets the caller say the scroll area's height is the
content's (an `h_auto` area rather than `size_full`), and a `scrollbar()` that puts its layer
outside the element it measures. Then a scroll box that decides its own height is one call
again.
## A `group_hover` on an element with no id of its own

gpui resolves a child's `group_hover` either against the group hitbox on the current frame's
paint stack or, at prepaint time — when that stack is empty — against the child's own hover
state, and that state only exists for elements the window keeps state for. An element with no
`ElementId` therefore carries a `group_hover` that silently never fires. The history row's
resume arrow is the case here: `.resume-arrow` takes the page's ink while the row is hovered,
and the glyph only started doing that once it had a name of its own (measured in the probe:
with no id the glyph stayed at the muted ink, with one it flipped to the page's, both themes).

To close it: hover state for elements without ids, or a documented requirement that a
`group_hover` carries one. The transcript's copy button and `agent_list`'s stop button both
name their own elements, which is what makes their group styles work.

## Completion: what a half-typed word could become (no endpoint)

The composer's popup asks two different things, and only one of them the protocol
answers as a read. The commands are `GET /catalog`'s own `commands[]` — that half
is fine. The **symbols** of `/eval`'s content are the live image's own knowledge
(which package the content reads in, which symbols are worth offering), and there
is no endpoint for them: the only way to ask is the `eval` op, with a form this
client composes.

So `crates/session/src/symbols.rs` builds one fixed form per token —

```lisp
(labels ((clean (s) …)) (dolist (row (evo.eval:completions-for "TOKEN")) (format t …)))
```

— the token goes in as a string literal with its quote and backslash escaped, and
the rows are read back off the op's **`output`** (`name<TAB>description` a line),
not off its `value`, which arrives wrapped in the op's own `⇒` decoration. Two
things follow from riding an op meant for something else:

- `--no-http-eval` servers refuse it, and the popup has nothing to offer there —
  silently, because a refusal is not something to say above the reader's typing;
- every such completion is an evaluation *in the reader's image*, with whatever
  printing the image does along the way; lines that are not the form's own are
  dropped rather than parsed into a candidate.

To close it: a read that answers a token — `POST /ops {"op":"complete","args":
{"token":…}}`, or a `GET /catalog?complete=…` — returning `{"name","description"}`
rows as JSON, marked read-only and honest about states where it cannot answer.

## `command.run`'s notices carry their style, not their text (evo bug, 2026-10)

`POST /ops {"op":"command.run"}` answers with `notices: [{severity, text}]`, and
every `text` is the *style keyword* of the line rather than the line: for
`/eval (+ 1 2)`, a real `evo-agent serve` answers

```json
{"notices": [{"severity": "info", "text": "plain"}]}
```

`op-command-run` (evo-agent `src/serve/ops.lisp`) builds those from
`reply-output`, whose entries are the plist `(:style :plain :text "⇒ 3")`, by
destructuring each as a flat `(style text)` list — so `style` is `:style` and
`text` is `:plain`. A client that drew them would print the registry's own
vocabulary.

The GUI therefore shows nothing from that list: a command's output is the
session's `notice` items (`evo.command:host-notice` publishes one as well, and
`docs/PROMPT.md` §5 has the client rendering `notice` items already), which the
transcript draws in the words the command wrote. A command's `data` and `choices`
are ignored for the same reason — a picker has no surface here (the model drawer
and the empty tab's choosers cover the one evo offers today), and the choice's own
title arrives as a notice.

To close it: `(destructuring-bind (&key style text) entry …)`, or `notices` as
objects (`{"severity","text"}`) built where the reply is, and a GUI that can draw
the list knowing its rows mean what they say.
