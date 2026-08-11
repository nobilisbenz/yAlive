# Ideas worth doing

Companion to [`PLAN.md`](PLAN.md). That document is the strategy; this one is the
shortlist — what to build, why it earns its place, and what it costs.

Two topics: why the graph comes before embeddings and what to steal for it, and the
dock stack decision (**iced 0.14** — Slint removed).

---

# Part 1 — Graph retrieval before embeddings

## 1.1 The two independent votes

`yy/plan/06-stage-5-semantic.md` schedules embeddings as Stage 5: a second `llama-server`,
Qwen3-Embedding-0.6B at ~650 MB VRAM, a `section_embeddings` table, a background embedding
queue, a re-embed-on-model-change path, brute-force cosine search, and an RRF fusion
tuning problem. That is the single largest subsystem left in the plan.

Two unrelated projects, built by different people for different domains, both concluded it
is the wrong first move:

| | [Graphify](https://github.com/Graphify-Labs/graphify) | [Semantica](https://github.com/semantica-agi/semantica) |
|---|---|---|
| Domain | Codebases + docs | Regulated-industry audit trails |
| Stance | *"Not a vector index. No embeddings, no vector store."* | *"structured, queryable context … not just a vector index"* |
| Method | Graph traversal, Leiden communities, degree ranking | Typed graph, provenance, deterministic reasoning |
| Evidence | **49.7% on LOCOMO — matching dense RAG**, at zero index cost | 118k-node production graph |

Neither is a personal-notes app and neither stack should be copied. What transfers is the
finding: **on a corpus with real structure, traversing a typed graph is competitive with
dense retrieval and costs a rounding error to run.**

## 1.2 Why this lands harder here than it did for them

Both projects have to *manufacture* their graph. Graphify runs tree-sitter over source and
tags every edge `EXTRACTED` or `INFERRED`. Semantica runs NER, relation extraction, and
conflict resolution before anything enters the store. Extraction quality is their central
engineering problem.

You skip that problem entirely. The graph is already there, hand-authored, every edge
typed and trusted:

```sql
relations(source_section_id, target_section_uid, relation_type, context)
-- related | outgoing | ingoing | contradicts | example-of
```

A `contradicts::` edge you wrote yourself is better signal than anything an extraction
pipeline produces, because it encodes a judgement no model had access to. **This is the
most valuable asset in the repository and retrieval currently does not read it.**

Cost comparison for the interactive path:

```
embeddings   →  650 MB VRAM, a second server, an async queue, a re-embed path
graph        →  two indexed SQL queries
```

**Do the cheap one first. Let the benchmark decide whether the expensive one ever ships.**

## 1.3 The five transfers

Ranked by payoff over effort. All five are small.

| # | Idea | Effort | Payoff | Lands in |
|---|---|---|---|---|
| 1 | `supersedes` as a relation type | ~10 lines | Large | Phase A.6 |
| 2 | Provenance rows + answer rating | ~1 day | Large — unblocks the benchmark | Phase B.13 |
| 3 | Subchunk → parent resolution | ~1 join | Prevents a silent bug | Before Phase D |
| 4 | Contradiction clusters | ~half a day | Medium | Phase D.22 |
| 5 | "Why this result" line | ~1 hour | Medium — and it debugs Phase D | Phase D.21 |

### 1 — Make `supersedes` a relation type

Semantica separates **valid time** (when a fact was true) from **recorded time** (when it
was learned). For a vault that accumulates five years of how-tos, that is the difference
between an assistant that helps and one that confidently recites a workflow you abandoned
in 2023.

You already hold both halves and they are not connected to each other:

- `brain-dock-spec.md` §12/§47 defines a `supersedes` frontmatter field.
- `config/brain.example.toml` already has `[search.status_weight] obsolete = 0.25`.

Add `supersedes` to the `relation_type` set alongside `related | outgoing | ingoing |
contradicts | example-of`. Every mechanism in `PLAN.md` §2.3 then handles staleness for
free — expansion demotes the superseded section and promotes its replacement — with no new
code path and no new table.

Ten lines for the largest single ranking improvement available. Do it in Phase A so the
corpus is authored with it from the start.

*Later, optionally:* nullable `valid_from` / `valid_until` on `sections`, alongside the
existing `created_at` / `modified_at`. That buys "how did I **used to** do X" as a distinct
answerable question. Build it only when you notice yourself wanting it.

### 2 — Provenance rows, which quietly solve the benchmark problem

Semantica makes decisions first-class graph nodes with full W3C PROV-O lineage. At your
scale that is compliance theatre. The useful slice is **one table**: per answer, record the
query, the `section_uid`s packed into the prompt, the model, and the timestamp.

- Stage 6 (corrections) needs exactly this to know *what* it is correcting.
- **It replaces the hand-labelled benchmark.** `PLAN.md` Phase B calls for sitting down and
  inventing 30–50 questions with known-correct sections. Instead: log real queries with the
  sections used, add one keystroke in the dock to mark an answer good or bad, and after two
  weeks of ordinary use you have a labelled retrieval set built from **your actual
  questions** — strictly better data than anything you would invent, and it arrives as a
  side effect of using the tool.

This converts the most tedious blocker in the plan into a background process. Land it early
in Phase B so labels accumulate while you build C and D.

### 3 — Subchunk → parent resolution

Semantica's splitter preserves entity and relation-triplet boundaries across chunks.
`yy` Stage 1 §1.4 splits oversized sections into ~450-token subchunks on paragraph
boundaries with ~60 tokens of overlap — and says nothing about what happens to the
section's relations.

The bug that follows: a 900-token section with all its `[[links]]` in the opening
paragraph splits into three subchunks. Subchunk 3 gets retrieved. Graph expansion from it
finds **nothing**, because every edge lives on subchunk 1. Graph retrieval silently
degrades to lexical for precisely the long, link-dense sections where it should be
strongest — and it fails *quietly*, which is the worst property a retrieval bug can have.

Fix, cheapest first:

- **(a)** expansion always resolves a subchunk to its parent section before traversing —
  one join, no schema change; or
- **(b)** subchunks inherit the parent's relation set — more rows, more precise attribution.

Decide before writing Phase D, not after debugging it.

### 4 — Contradiction clusters

Semantica flags contradictory facts *before* they enter the graph rather than storing both
and hoping. You already have both halves: a `diagnostics(path, line, message)` table, and
broken-link detection at `src/db.rs:214`.

At index time, find pairs joined by `contradicts::` where **neither** side is marked
`obsolete`, `archived`, or superseded. That is an unresolved disagreement with yourself,
and it is exactly what makes the dock answer confidently and wrongly. Report it in
`brainctl doctor` as vault health.

Second payoff: an unresolved contradiction is an excellent flashcard. A `yReviewy` feature
falls out of a retrieval fix.

### 5 — "Why this result"

Semantica's decision-aware retrieval explains why a result was selected. One line under the
source badge:

```
OBS > Cursor follow > Smoothing        matched heading · 1 hop from current note
```

An hour of work, and it is the debugger for Phase D — when graph retrieval returns the
wrong section you can see immediately whether the seed was wrong or the expansion was.

## 1.4 Do not take

RDF, SPARQL, OWL/SHACL governance, Neo4j, Rete networks, Datalog, polyglot storage
abstractions, LiteLLM, audit-trail export, entity-merge pipelines.

Also: Semantica's headline **"6,000× faster node search (24 ms → 0.004 ms)"** on a 118k-node
graph is what adding an index looks like. It is `PLAN.md` §3.2's missing
`relations_target` index with a bigger number attached, not a new technique — though it is
a useful preview of what that missing index will cost you at scale.

---

# Part 2 — The dock stack: **iced 0.14, decided**

Slint is out. The dock, and eventually `yGraphy`, run on **iced 0.14 + `iced_wgpu`**.

This section previously argued for keeping Slint, then — once the graph panel became a
requirement — for egui over iced. The decision went to iced on developer experience, which
is the author's call to make and a legitimate basis for it: a toolkit you enjoy is one you
will actually finish. Having then checked iced 0.14 properly, **two of the three arguments
I made against it were wrong.** Recorded below, because a plan that hides its corrections
is not usable as a reference.

## 2.1 Two corrections

**Wrong: "only egui hands you a raw `CommandEncoder`."** iced's `shader::Primitive` gives
you both:

| Method | Receives |
|---|---|
| `prepare()` | `&mut Self::Pipeline`, `&Device`, `&Queue`, bounds, viewport |
| `draw()` | `&mut RenderPass` |
| `render()` | `&mut CommandEncoder`, `&TextureView`, clip bounds |

`render()` is full control — your own passes into your own target. `yGraphy`'s existing
renderer maps onto this almost structurally: its `Renderer` becomes the `Pipeline`
associated type, buffer uploads go in `prepare()`, the instanced circle and line draws go
in `draw()`.

**Wrong: "iced hides window lifecycle, so map/unmap and pre-map `WM_CLASS` mean fighting
it."** iced 0.14 exposes all of it as stable API:

- `window::Settings.platform_specific.application_id` → `WM_CLASS`, set at creation, which
  is what i3's map-time `for_window` matching requires.
- `window::set_mode(id, Mode::Hidden)` → show/hide without destroying the window.
  Slint's `hide()` **did** destroy it — a measured finding in Stage 0 — and forced the
  manual `x11rb` map/unmap that `brain-x11` carries today.
- `window::Settings.transparent`, `decorations`, `level`.
- `iced::window::raw_id()` for the XID when X11 needs driving directly.
- `iced::daemon()` — a runtime that starts windowless and does not exit when windows close.

**Right, and unchanged: iced has the better presentation layer.** That was listed as its
one advantage. It remains true, and it is now on the side we are building.

The net: the dock lands on **entirely stable APIs**, dropping `unstable-winit-030`,
`raw-window-handle-06`, and the prospective `unstable-wgpu-28`. Fewer unstable
dependencies than the stack it replaces.

## 2.2 What this costs

- Rewrite `brain-dock` (~535 lines in `main.rs`, plus `ipc.rs`, `keys.rs`, `stream.rs`,
  `window.rs`, `platform.rs`); replace `ui/*.slint` with iced views. **One to two weeks.**
- **Re-prove Stage 0.** The 10–15 ms summon, transparency under picom, `WM_CLASS` before
  map, and the focus grab are measured facts about the *Slint* build. They are unmeasured
  again until re-run.
- `ui/tokens.slint` → `ui/tokens.rs`. Same invariant — every colour, radius, and duration
  in exactly one place — new home, enforced by review rather than by the compiler.
- `yGraphy` moves **wgpu 23 → 27** to match `iced_wgpu` 0.14. `winit` 0.30 already matches.

**Untouched:** `brain-core`, `brain-proto`, `brain-daemon`, `brainctl`, and nearly all of
`brain-x11` — `geometry.rs`, `atoms.rs`, and most of `dock_window.rs` are about i3 and
X11, not about any toolkit. The i3 findings from Stage 0 (no `_NET_WORKAREA`;
`_NET_WM_STATE_ABOVE` ignored; three-level tree walk; `ChangeProperty` when unmapped,
`ClientMessage` when mapped) all survive intact.

## 2.3 The one open technical risk

`Mode::Hidden` *should* be winit `set_visible(false)`, i.e. an X11 unmap that preserves the
window. That is inference. Stage 0 already lost time to exactly this assumption being
wrong in Slint, so **spike it before writing UI code**: does
`Mode::Hidden` ⇄ `Mode::Windowed` preserve the XID, keep the wgpu surface valid, and
round-trip inside 50 ms?

If not, the fallback is already proven in this codebase — take the XID from
`window::raw_id()` and drive `map_window`/`unmap_window` with `x11rb`, exactly as
`brain-x11` does now. Toolkit-independent, and known to work.

## 2.4 The knock-on: `glyphon` has no wgpu-27 release

```
iced 0.14    → wgpu 27
glyphon 0.9  → wgpu 25
glyphon 0.10 → wgpu 28      ← nothing targets 27
```

`yGraphy` is on `glyphon 0.7`, so its text layer cannot come across unchanged. Options:
patch `glyphon`; pin both dependencies to a pair that lines up; or **drop `glyphon` and
render node labels as iced `text` widgets** stacked over the shader widget, positioned by
projecting graph coordinates through the camera.

The third is probably right — the embedded panel shows a *subgraph* of tens of nodes, so a
few dozen text widgets is nothing, and the labels get iced's font rendering and theming for
free. The catch is `yGraphy`'s full-vault mode, where thousands of labels is too many
widgets — solvable by drawing labels only above a zoom threshold and only for visible
nodes, which a full-vault view should do anyway. **Decide deliberately** (`PLAN.md` §5).

## 2.5 Requirements that survive the change

- **The daemon owns visibility.** `brainctl graph toggle` → daemon flips `graph_visible` →
  the dock shows the panel. `brainctl` stays stateless.
- **Render on demand.** `yGraphy` runs its force simulation forever today. In a window
  resident from login that is a permanent GPU cost. iced is event-driven by default, so
  this is now the natural behaviour rather than a fight — but the simulation still has to
  be told to converge and stop.
- **Seed contextually.** The panel opens on the current primary source's `section_uid` and
  renders the subgraph Phase D packed into the prompt — not the whole vault. That is what
  makes it a feature and the retrieval debugger at once.

---

Sources: [Graphify](https://github.com/Graphify-Labs/graphify) ·
[Semantica](https://github.com/semantica-agi/semantica) ·
[iced::widget::shader](https://docs.rs/iced/latest/iced/widget/shader/index.html) ·
[shader::Primitive](https://docs.rs/iced_widget/0.14.0/iced_widget/shader/trait.Primitive.html) ·
[iced::window::Mode](https://docs.rs/iced/latest/iced/window/enum.Mode.html) ·
[iced::daemon](https://docs.rs/iced/latest/iced/fn.daemon.html) ·
[iced::window::Settings](https://docs.rs/iced/latest/iced/window/struct.Settings.html)
