# yalive — next steps

Scope: the superproject and its three submodules. Covers the UI stack decision, how
`yGraphy` and `yy` become one system, and the work that makes `yy` fast.

**Settled: the dock is iced 0.14. Slint is removed from the project** (§1).

Written against: `yalive` @ `src/{db,parser,model,graph}.rs`, `yy` @ Stage 0 complete on
iced and Stage 1 begun, `yGraphy` @ iced + wgpu 27, `yReviewy` @ Tauri 2 (unchanged).

Phases A, A′, and E have landed; each item below carries its own status.

---

## 0. The one number that reframes everything

```
/home/nabi/brain/.notes/index.sqlite   →  50 files, 190 sections, 73 relations
/home/nabi/alive/.notes/index.sqlite   →   5 files,   5 sections,  4 relations
```

`~/brain` is now a real corpus — 50 linked notes — and `~/alive` is still a fixture. That
is enough to make retrieval *quality* judgeable, which it was not before, and it is what
turned up the two ranking bugs recorded in `yy/plan/09-decisions.md` §12. It is **not**
enough to make latency claims mean much: measured end to end over the socket,

```
brainctl bench   p50 0.4 ms   p99 0.6 ms   (retrieval only, 190 sections)
```

against a 100 ms target. That number says the pipeline is not doing anything stupid; it
does not say what happens at 40k sections. Growing the corpus by another order of magnitude
is still the thing that would make §3.2 falsifiable.

Generation is now measured rather than predicted, and it came in far better than this
document assumed:

```
                        predicted here          measured (Phase C)
prefill / TTFT          ~310 ms                 24 ms p50, 42 ms p99
generation              ~2100 ms (350 tok)      ~600 ms (~90 tok at 155.6 tok/s)
retrieval               91 ms (mock)            0.4 ms
total                   ~2.5 s                  288 ms p50, 703 ms p99
```

The prediction was wrong in two places, both worth keeping in mind. **Prefill effectively
disappeared** — a byte-identical prompt prefix plus `--cache-reuse 256` means the ~2000-token
context pack is prefilled once per session, not once per query, so the 310 ms estimate only
describes the *first* query (measured: 182 ms). And **the answer is a third of the assumed
length**, because the three-sentence contract was adopted; 350 tokens was never a target, it
was a cap.

**Everything is now a rounding error next to a 288 ms total.** The consequence is that the
remaining latency ideas in §3.1 — chiefly speculative decoding — are optimising a path that
is already fast, and should be judged on that basis rather than on the numbers above them.

---

## 1. The dock stack — **decided: iced 0.14. Slint is removed.**

The dock moves to **iced 0.14 + `iced_wgpu`**. Slint comes out of the project entirely.

This reverses an earlier recommendation in this document to keep Slint. The decision is
the author's, made on developer experience — and once the graph panel became a
requirement, the technical case moved to meet it. What follows is what iced actually gives
us, what it costs, and what carries over.

### 1.1 iced 0.14 covers all three hard requirements with stable APIs

The dock's window behaviour is unusual and it is what previously made Slint look
unavoidable. Verified against iced 0.14:

| Requirement | Slint | **iced 0.14** |
|---|---|---|
| `WM_CLASS` set **before first map** (i3 evaluates `for_window` at map time) | `unstable-winit-030` window-attributes hook | `window::Settings.platform_specific.application_id` — **stable, first-class** |
| Show/hide **without destroying the window** | Slint `hide()`/`show()` *destroys it and exits the event loop* (measured — Stage 0 header table). Required manual `x11rb` map/unmap. | `window::set_mode(id, Mode::Hidden)` — **stable**. `Mode` is `Windowed \| Fullscreen \| Hidden`. |
| Custom wgpu (the graph panel) | `unstable-wgpu-28`, which forces the whole renderer onto wgpu and risks the ARGB transparency | `iced::widget::shader` behind the default `wgpu` feature — **stable** |
| Transparent ARGB window | `background: transparent` + winit request | `window::Settings.transparent = true` |
| Raw XID when we need X11 directly | `raw-window-handle-06` | `window::raw_id() -> Task<u64>`, plus `window::run(id, f)` for the full handle |
| Runtime that survives with no window open | n/a | `iced::daemon()` — starts windowless, does not exit when windows close |

**All three unstable Slint features disappear.** The dock ends up on entirely stable,
documented APIs. That is a real reduction in fragility, not a consolation prize — and it
is the opposite of what §1 originally predicted.

`iced::daemon()` deserves particular note: it is a runtime that starts with no window and
does not quit when its windows close. That is a closer match to "resident from login,
summoned on a keystroke" than anything Slint offered.

### 1.2 What carries over unchanged

Most of Stage 0's hard-won knowledge is about **i3 and X11**, not about Slint. It all
survives:

- `_NET_WORKAREA` is not published by i3 → derive from `_NET_WM_STRUT_PARTIAL`, walking
  **three** levels because dock windows are i3's grandchildren.
- `_NET_WM_STATE_ABOVE` is ignored by i3 → floating plus an explicit `ConfigureWindow`
  raise is what works.
- `focus_on_window_activation smart` is mandatory, and `_NET_ACTIVE_WINDOW` must be sent
  as a `ClientMessage` with `data[0] = 2`.
- Property-setting rule: `ChangeProperty` while unmapped, `ClientMessage` to root while
  mapped. Getting it backwards is the classic intermittent always-on-top bug.
- Depth-32 ARGB visual, picom on `egl`.
- Never `override_redirect`.
- RandR monitor selection and the anchor maths.

Concretely: **`brain-x11` survives almost intact** — `geometry.rs` (392 lines), `atoms.rs`,
and most of `dock_window.rs` (343) are toolkit-agnostic. Only XID acquisition changes, and
`brain-core`, `brain-proto`, `brain-daemon`, and `brainctl` are untouched. The rewrite is
`brain-dock` and the `ui/` directory, nothing else.

### 1.3 What it costs

Do not pretend this is free:

- **Rewrite `brain-dock`** (~535 lines in `main.rs` plus `ipc.rs`, `keys.rs`, `stream.rs`,
  `window.rs`, `platform.rs`) and replace `ui/*.slint` with iced views. Call it
  **one to two weeks** to parity.
- **Re-prove Stage 0.** Transparency under picom, `WM_CLASS` before map, focus grab,
  and the 10–15 ms summon are all measured facts about the *Slint* build. They become
  unmeasured again. Budget for one of them being harder here.
- **The `tokens.slint` invariant needs a new home.** The project rule — *"visual constants
  live in `ui/tokens.slint`; no colour, radius, or duration in Rust"* — cannot survive
  literally. Replace it, do not drop it: a single `ui/tokens.rs` (or an iced `Theme`
  implementation) holding every colour, radius, and duration, enforced by review.
- **The threading model inverts, and simplifies.** Slint required a main-thread event loop
  with `invoke_from_event_loop` marshalling from Tokio. iced's Elm loop takes
  `Task`/`Subscription` instead — the daemon's event stream becomes a `Subscription`, which
  is a better fit than the callback marshalling it replaces. Token batching (~30 ms tick or
  ~24 chars) is still required, for the same reason.

### 1.4 The one thing to verify first

`Mode::Hidden` maps to winit `set_visible(false)`, which on X11 is an unmap — it should
*not* destroy the window the way Slint's `hide()` did. But that is inference, and Stage 0
already burned time on exactly this assumption being wrong in the other toolkit.

**Spike it before writing UI code** (Phase A.7): does
`window::set_mode(id, Mode::Hidden)` → `Mode::Windowed` round-trip preserve the XID and
land inside 50 ms, with the wgpu surface intact?

If it does not, the fallback is the one already proven in this codebase: take the XID via
`window::raw_id()` and drive `map_window`/`unmap_window` with `x11rb` directly,
exactly as `brain-x11` does today. That path is known to work and is toolkit-independent.

### 1.5 The other two apps

- `yGraphy` → **folds into the iced stack** (§7). It is already winit 0.30, which iced 0.14
  also uses. Its wgpu must go **23 → 27** to match `iced_wgpu` 0.14.
- `yReviewy` → **unchanged. Tauri + TS.** Android, touch, animated card flips. The one
  place a webview is the right answer, and nothing here touches it.

The thing that still needs unifying is **not the UI stack — it is the index.** That is §2,
and it is unaffected by this decision.

---

## 2. Q1b — Making `yGraphy` and `yy` one system

### 2.1 The actual problem: `yy` is about to build a second index

`yGraphy` gets this right already:

```toml
# yGraphy/Cargo.toml
yalive = { path = ".." }
```

```rust
use yalive::db::Database;
use yalive::model::GraphData;
```

It links the engine and reads the vault. One parser, one schema, one identity.

`yy` is a **separate cargo workspace** whose Stage 1 plan calls for building all of it
again: its own `migrations/001_initial.sql`, its own `sections` table, its own
`pulldown-cmark` parser, its own FTS5 with different tokenizer and different BM25 weights,
its own watcher. Today `brain-index/src/lib.rs` and `brain-engine/src/lib.rs` are one
line each — **nothing has been written yet.** This is the last cheap moment to decide.

Two divergences that become permanent the day Stage 1 lands:

**Section identity.** `yalive` keys everything on `section_uid` — a stable string that
`relations`, `cards`, and `review_state` all hang off. `yy` plans to key on
`sections.id` + `heading_path`. Two identities means the graph cannot be shared, review
state cannot inform ranking, and `Alt+1` jumps to a line number computed by a *different
parser* than the one that built the graph you are looking at in `yGraphy`.

**Full-text search.** `yalive` uses a **standalone** FTS5 table hand-synced in two places
(`src/db.rs:297`, `src/db.rs:1593`), storing `body` twice, weighted
`bm25(section_search, 0, 2, 5, 1, 1)`. `yy` plans **external-content** FTS5 with triggers,
`tokenchars '_-.'`, weighted `8, 4, 1`. Both cannot be the source of truth.

### 2.2 The decision

| | Approach | Verdict |
|---|---|---|
| **A** | `yy` depends on `yalive` as its index layer. `brain-index` becomes a thin wrapper over `yalive::db` + `yalive::parser`, not a reimplementation. | **Recommended** |
| **B** | `yy` keeps its own DB but mirrors `section_uid` on every vault-sourced row, so the graph and review state can be joined across. | Acceptable fallback |
| **C** | Full duplication as currently planned. | Reject |

**Recommendation: A.** Add `yalive` as a path dependency of the `yy` workspace and spend
Stage 1 *upgrading `src/db.rs`* rather than writing a second one. Everything in `yy`'s
Stage 1 plan that is genuinely better than what `yalive` has today — `tokenchars '_-.'`,
external-content + triggers, a migration runner, FTS5 query escaping, `heading_path`,
the one-writer/N-reader connection strategy — becomes an improvement to `yalive` that
`yGraphy` and `yReviewy` inherit for free. That is the same amount of work, spent once.

**The honest complication**, and the reason B exists: `yy`'s spec deliberately indexes
*more than a vault* — `~/projects`, code, PDFs, up to 50k files — while `yalive` is a
Markdown vault indexer with one `.notes/index.sqlite` per vault. The merge is not literal.

The resolution: **`yalive` owns the vault schema and section identity; `yy` owns the
non-vault superset.** Two databases is fine. What is not fine is two definitions of what
a section is. Concretely, `yy` opens the vault DB read-only through `yalive::db` for
everything vault-shaped, and keeps a second DB for code/PDF sources whose rows carry
`source_kind` and no `section_uid`. Retrieval fuses results from both; only vault rows
participate in graph expansion and review-state ranking.

### 2.3 Graph-first retrieval — what to actually take from Graphify

[Graphify](https://github.com/Graphify-Labs/graphify) is explicit that it is *not* a
vector index: no embeddings, no vector store. It traverses a real typed graph — shortest
path, Leiden communities, degree ranking ("god nodes"), semantic subgraph scoping — and
reports **49.7% on LOCOMO, matching dense RAG**, at zero indexing cost.

The reason that result transfers to `yalive` and not to most projects: Graphify has to
*extract* its graph from source with tree-sitter and tag edges `EXTRACTED` vs `INFERRED`.
**You already have the graph, hand-authored, and every edge is typed and trusted:**

```sql
relations(source_section_id, target_section_uid, relation_type, context)
-- related | outgoing | ingoing | contradicts | example-of
```

A hand-written `contradicts::` edge is higher-quality signal than anything an LLM
extraction pass produces. This is the strongest asset in the codebase and retrieval
currently ignores it entirely.

**Consequence for `yy`'s roadmap: Stage 5 (embeddings) should be demoted below graph
retrieval, and possibly skipped.** It costs 650 MB of VRAM, a second `llama-server`, an
embedding queue, a re-embed-on-model-change path, and a fusion tuning problem. Graph
expansion costs two indexed SQL queries. Do the cheap one first and let the benchmark
decide whether the expensive one earns its place.

**The retrieval pipeline: seed → expand → rank → pack.**

1. **Seed** — FTS5 BM25, `lexical_limit = 30`. Already fast, already planned.
2. **Expand** — 1–2 hop traversal over `relations` from the seed sections, both
   directions. Backlinks matter as much as forward links.
3. **Rank** — reuse the RRF machinery already chosen for lexical+semantic
   (`k = 60`), but fuse *graph rank* in place of *semantic rank*. Combine **ranks, not
   scores** — the reasoning in Stage 5 §5.5 is correct and applies unchanged. Signals:
   - hop decay — 1-hop ≈ 0.6, 2-hop ≈ 0.35
   - **precomputed** degree / PageRank prior, stored on the row (§4.2)
   - relation-type weights: `contradicts` boosts hard — surfacing the note that corrects
     an older one is exactly the failure mode a personal vault has; `example-of` boosts
     on "how do I" phrasing; `ingoing` reads as authority
   - **review state** — nobody else can do this. `review_state.stability` says you have
     internalised a section; `lapse_count` and an overdue `due_at` say you are actively
     confused about it. Both are free relevance priors sitting in the same database,
     and they point in useful opposite directions depending on question shape.
4. **Pack** — this is where "the graph makes the LLM faster" becomes literally true.
   Instead of five flat sections, send **one subgraph**: the seed section in full, its
   heading path, and its typed neighbours as one-line stubs
   (`contradicts → OBS > Cursor follow > Smoothing`). Same information, a fraction of the
   tokens, and the structure tells a 1.7B model how the pieces relate instead of making
   it infer that. Fewer prefill tokens *and* better grounding — the rare change that
   improves both axes.

Also worth taking: **communities.** Label propagation or Leiden over `relations`, stored
as `sections.community_id`, recomputed on index-generation bump. A strong seed then boosts
its whole community cheaply. `yGraphy` already computes something adjacent in
`TopicGroup` / `LayoutGraph` — which is the natural bridge to the next section.

### 2.4 Three levels of `yGraphy` ↔ `yy` integration

**L1 — shared graph code. Do this first; it is the whole point.**

`yGraphy/src/main.rs` is ~1300 lines in one file: `LayoutGraph`, `SectionNode`,
`NoteGroup`, `TopicGroup`, force simulation, and the wgpu renderer all together. The
graph *algorithms* in there are exactly what `yy`'s retrieval needs and have nothing to
do with rendering.

Extract into `yalive::graph`:

- adjacency build from `relations` (forward + reverse)
- degree / PageRank
- k-hop expansion with decay
- community assignment

`yGraphy` keeps layout and rendering and calls into it. `yy` calls the same functions for
retrieval. One implementation, and `yGraphy` becomes a *visual debugger for the retrieval
graph* — when a query returns the wrong section, you can look at why.

**L2 — bidirectional focus. Cheap, high perceived value.**

`yGraphy` already has `focus_in_tui(vault, uid)` — graph → TUI works. Build the inverse:

- `yGraphy --focus <section_uid>` — launch or focus centred on a node.
- A `Show in graph` action button in the dock, as an `ActionKind` variant. This respects
  the invariant: the button comes from the retrieved section's `section_uid`, which is
  parsed metadata, never model output. No new attack surface.
- `brain-proto` already carries a Unix socket + JSON Lines; `ServerEvent::Sources` just
  needs to carry `section_uid` alongside path and line.

**L3 — graph inside the dock. Now the default plan, not an optional extra.**

On iced this is a `shader` widget rather than a cross-toolkit texture handoff, so L3 stops
being speculative. Full detail in §7. L2 stays worth building first regardless: it works
standalone, it is a day of work, and it is the fallback if the panel slips.

---

## 3. Q2 — Making `yy` blazingly fast

Re-read §0 first. Ranked by milliseconds a user can actually feel.

### 3.1 Tier 1 — generation latency (~75% of wall clock)

This is where the wins are. All of it is Stage 2 work.

- **Prefix-stable prompts + KV cache reuse.** Run `llama-server` with `--cache-reuse N`
  and keep the system prompt and instruction block **byte-identical across every query**,
  with retrieved context strictly after it. Then prefill only ever touches the changed
  suffix instead of re-processing ~2000 tokens at 6555 t/s. This alone reclaims most of
  the ~310 ms prefill. It costs nothing but discipline in prompt assembly — and it is very
  easy to lose accidentally by interpolating a timestamp or a query id into the prefix.

- **Speculative decoding.** The single highest-leverage idea in this document. Generation
  is 2.1 s of the 2.7 s budget. Draft with **Qwen3-0.6B** (`--model-draft`) against the
  1.7B target: VRAM is 1.3 + 0.4 GB against 6 GB, comfortable. Draft acceptance is
  normally the risk — here it is the advantage, because the prompt contract deliberately
  makes the model *restate retrieved text*. Highly predictable output is exactly the
  regime where speculation wins. Expect a large fraction of the generation time back.
  Benchmark it; do not assume it.

- **Cut `max_output_tokens` 350 → ~200** and instruct for two or three sentences.
  350 tokens at 168 t/s is 2.1 s; 200 is 1.2 s. **Nine hundred milliseconds for a prompt
  edit.** The source badge and jump action are already on screen — per the project's own
  framing, prose is the bonus, not the product.

- **Warm on daemon start.** `keep_loaded = true` is already in config. Also fire a
  one-token request at startup so CUDA context, graphs, and allocator are hot before the
  first real query, not during it.

- **Keep §3 and §4 of `09-decisions.md`** — sources before generation, live search on an
  80 ms debounce. These are perceived-latency wins that dwarf most real ones.

### 3.2 Tier 2 — retrieval and index

Do these because they are *correct*, and because graph expansion in the hot path will not
survive without them. Not because 5 rows are slow.

- **Add the missing indices.** `src/db.rs` contains **zero** `CREATE INDEX` statements.
  Everything currently rides on primary keys and `UNIQUE` constraints. Notably:

  ```sql
  -- relations PK is (source_section_id, target_section_uid, relation_type):
  -- forward traversal is covered, REVERSE IS A FULL TABLE SCAN.
  -- src/db.rs:960 and :970 both filter on r.target_section_uid.
  CREATE INDEX IF NOT EXISTS relations_target ON relations(target_section_uid);
  CREATE INDEX IF NOT EXISTS sections_file    ON sections(file_id);
  CREATE INDEX IF NOT EXISTS cards_section    ON cards(section_uid);
  ```

  Backlink expansion is half of §2.3 step 2. At 4 rows it is free; at 40k sections it is
  O(n) per hop, per query, in the interactive path. Add these **before** building graph
  retrieval on top of them.

- **Move FTS5 to external-content + triggers.** Removes the duplicate storage of every
  section `body`, and removes the hand-sync in two separate call sites — the failure mode
  where one path forgets and search silently serves stale rows for weeks. Adopt
  `tokenize="unicode61 remove_diacritics 2 tokenchars '_-.'"` at the same time; on a vault
  containing code, keeping `calculate_pivot` as one token is a large precision win for one
  tokenizer option. Run `INSERT INTO section_search(section_search) VALUES('optimize')`
  after a full reindex.

- **FTS5 query escaping** (`yy` Stage 1 §1.6). This is a **crash, not a slowdown** —
  `MATCH` takes an expression language, and the first query containing `?`, `-`, or `'`
  throws. Tokenise → quote → `OR`. Fuzz it. Ship it early.

- **PRAGMAs.** `src/db.rs:29` sets `foreign_keys` + WAL + a 3 s busy timeout. Add:

  ```sql
  PRAGMA synchronous = NORMAL;   -- correct pairing with WAL; FULL costs an fsync per commit
  PRAGMA cache_size  = -32000;   -- 32 MB
  PRAGMA temp_store  = MEMORY;
  PRAGMA mmap_size   = 268435456;
  ```

- **`prepare_cached`.** The hot query path re-prepares statements on every call today.

- **One writer thread + reader pool** (`yy` Stage 1 §1.3). Correct as written — but apply
  it to `yalive::db::Database`, which is currently a single `Connection` with no pool.
  All DB calls from async code go through `spawn_blocking`, or a reindex stalls the runtime.

- **Precompute graph analytics.** `sections.rank` (degree/PageRank) and
  `sections.community_id`, written during indexing, bumped with `index_generation`. The
  interactive path must never run graph analytics — only read columns.

### 3.3 Tier 3 — measurement, which gates all of the above

- **Build a real corpus.** Phase A.1. Import or generate a few thousand sections with
  realistic link density. Until then no performance claim in this repo means anything.
- **Benchmark harness early** (`09-decisions.md` deviation #9 already says this).
  30–50 real questions with a known-correct `section_uid`; report Recall@3 and MRR.
  Without it, "graph retrieval beats BM25" is an opinion. `yy/benchmarks/` is empty.
- **`tracing` spans per stage from the first commit**, carrying the query id — plus
  `brainctl bench` printing p50/p99 for retrieval, TTFT, and total. Retrofitting
  instrumentation after a latency complaint means guessing.
- **Criterion** for the parser and the FTS escape function.

### 3.4 Explicitly do not

- **Do not add embeddings** until the benchmark shows BM25 + graph missing real queries.
  §2.3 is the cheaper substitute and Graphify is evidence it can be competitive.
- **Do not add an ANN index.** Already decided correctly in `09-decisions.md` #7.
- **Do not rewrite the UI.** §1.
- **Do not tune FTS below ~20 ms.** It is 4% of a budget dominated by generation.

---

## 4. Sequence

### Phase A — before a single line of `yy` Stage 1
1. 🟨 **Build a real corpus.** `~/brain` is 50 notes / 190 sections / 73 relations, which
   is enough to judge ranking and was what surfaced the two bugs in `09-decisions.md` §12.
   Not yet enough to falsify a latency claim — that needs another order of magnitude.
2. ✅ **Decide §2.2** — **A**. `yy` wraps `yalive` as its index layer; `yalive` owns the
   vault schema and section identity, `yy` owns the non-vault superset.
3. ✅ Missing indices and PRAGMAs added to `src/db.rs` — `relations_target` (the reverse
   traversal that was a full scan), `sections_file`, `sections_parent`, `cards_section`;
   `synchronous=NORMAL`, `cache_size`, `temp_store`, `mmap_size`.
4. ✅ FTS5 is external-content + triggers + `tokenchars`, over a `section_content` **view**
   (so `note_title` and `tags`, which live on `files`, come along), with a `user_version`
   migration that rebuilds the index. `heading_path` is now a searchable column.

   The delete path needed two triggers that were not obvious, and both are load-bearing:
   `ON DELETE CASCADE` removes the `files` row *before* its sections, so the section
   trigger's subquery for the title finds nothing and the FTS `'delete'` command silently
   leaves the title tokens behind pointing at a dead rowid — and `integrity-check` does
   **not** catch that. Retitling has the mirror problem, since `replace_note` updates
   `files` before `sections`. Both are covered by a test that walks a note through edit,
   retitle, partial delete, and full delete.
5. ✅ `yalive::graph` extracted, and `yGraphy` binary → library (§2.4 L1). Turned out to be
   mostly new code: what was in `main.rs` was force layout and grouping, not the
   traversal retrieval needs. Adjacency (both directions), PageRank, Louvain communities,
   k-hop expansion with decay, contradiction pairs. `yGraphy` now draws the same edges
   retrieval walks.
6. ✅ `supersedes` added as a relation type, weighted just under `contradicts`.
7. ✅ **Spiked the iced window layer** — all four answers in
   `yy/plan/01-stage-0-dock.md` §0.0. No fallbacks needed.

*Also done, out of sequence because it was cheap and Stage 1 needs it:* `heading_path` is
now stored on `sections`, computed from the parser's existing heading stack.

### Phase A′ — the iced port (parallel with A, blocks nothing else)
8. `brain-dock` rewritten on iced 0.14: `iced::daemon()`, daemon events as a
   `Subscription`, token batching preserved (~30 ms / ~24 chars).
9. `ui/tokens.rs` replacing `ui/tokens.slint` — same invariant, new home.
10. Slint removed from `yy/Cargo.toml`, `ui/*.slint` deleted, `brain-dock/build.rs` dropped.
11. Re-run the Stage 0 definition of done. **Do not proceed until the 50 ms summon is
    measured again**, not assumed.

### Phase B — `yy` Stage 1, on top of `yalive` — **the DoD passes**
12. ✅ `brain-index` wraps `yalive::db` / `yalive::parser` rather than reimplementing them.
13. ✅ FTS5 query escaping + the hostile-query test, in `yalive::search` (§11 of
    `09-decisions.md` explains why it moved) — plus `Mode::All` / `Mode::Any` (§12).
14. ✅ Writer thread + elastic reader pool + `spawn_blocking`, with the graph snapshot
    republished by the writer so the query path never runs graph analytics.
15. 🟨 Sources and actions are emitted before anything else, and the daemon answers a
    retrieval-only query in ~0.4 ms. **Search-as-you-type is still not wired in the dock** —
    it sends on Enter.
16. ✅ `tracing` spans per stage + `brainctl bench` with p50/p99.
17. ✅ **Provenance rows + one keystroke to rate an answer** (§6.3). Every answer records
    its query, the `section_uid`s packed into the prompt, and the model; `Ctrl+G` / `Ctrl+B`
    in the dock rate it, and `brainctl status` reports how many rows have accumulated. The
    rated rows *are* the Phase D benchmark, built from questions actually asked.

*Also landed here, because retrieval needed them:* config loading and validation
(all problems reported at once, `$HOME` and `/` refused), front-matter `status:` parsed and
wired to `[search.status_weight]` — it had been configuring nothing — the file watcher on
`notify-debouncer-full`, openers with per-element argv expansion and detached spawn, and
`brainctl sources` / `doctor` / `bench`. Graph expansion is on by default and visibly
working: `1 hop back to related` shows up in real results.

### Phase C — `yy` Stage 2, LLM — **the DoD passes**
18. ✅ Prefix-stable prompt + `--cache-reuse 256`. The system block is a `const` and the
    retrieved context comes strictly after it; a test asserts the prefix never varies,
    because losing it costs a full reprefill per query and nothing fails visibly.
19. 🟨 Speculative decoding — **not measured, and the case for it has weakened.** See the
    numbers below: it was sized against a predicted 2.1 s of generation, and generation is
    now ~600 ms.
20. ✅ `max_output_tokens` → 200 with a three-sentence contract.
21. ✅ Warm-up request on daemon start, sending the real system block so the first query
    reuses the cached prefix too.

Measured, Qwen3-1.7B Q5_K_M on CUDA, 50-note vault:

```text
TTFT        p50  24 ms   p99  42 ms     (target < 500 ms; first query of a session: 182 ms)
generation          155.6 tok/s         (llama-bench ceiling 168 t/s)
total       p50 288 ms   p99 703 ms
```

*Also landed:* llama-server supervision with health checks and backoff restart (a killed
server is detected within 5 s and back in ~1.3 s), the confidence gate that decides
no-answer **before** the model is called, `<think>` stripping as a third line of defence,
degradation to lexical-only with the sources still on screen, and `brainctl bench
--generate`.

*Also landed:* the answer cache, keyed on a hash of the packed sections' bodies plus model,
prompt version, and generation params. A repeated question drops **618 ms → 3 ms** and
renders whole rather than replayed. Editing a note that fed an answer regenerates it;
appending an unrelated section does not. Both verified.

The store holding it is `yy`'s own SQLite at `$XDG_DATA_HOME/brain/brain.sqlite` — the
second store §2.2 anticipated. Deliberately not in the vault's `.notes/`, which `yalive`
owns and rebuilds from the Markdown whenever its schema changes; answers and provenance
would be lost by exactly that rebuild.

*Still open:* the in-memory retrieval cache. Retrieval is 0.4 ms, so it would save nothing
measurable.

### Phase D — graph retrieval
22. Precompute `rank` and `community_id` during indexing.
23. Subchunk → parent resolution before expansion (§6.4). Decide this *before* writing D.
24. seed → expand → rank → pack, behind a config flag.
25. Subgraph prompt packing + "why this result" line in the dock (§6.5).
26. Contradiction clusters in `brainctl doctor` (§6.2).
27. A/B against lexical-only on the Phase B benchmark. **Ship only if it wins.**

### Phase E — the graph panel ✅ landed
28. ✅ `yGraphy --focus <section_uid>` (§2.4 L2). The `Show in graph` **action button** is
    not wired — actions arrive in Stage 3 — but `SourceRef` now carries `section_uid`,
    which was the only missing piece.
29. ✅ `brainctl graph toggle`, daemon-owned `graph_visible`, replayed to a reconnecting
    dock only when open. Deliberately independent of dock visibility: the panel is a mode
    the user is in, not part of a summon, so dismissing the dock leaves it where it was.
    The X11 resize is not one-shot — the window height is recomputed by `layout.rs` the
    same way an answer's is, which turned out to need no new mechanism at all.
30. ✅ `ygraphy` as a `shader::Program`, embedded below the answer, seeded on the primary
    source's `section_uid`. Redraw is on-demand (`window::frames()` subscribed only while
    the simulation is unsettled) and the simulation settles after
    `GRAPH_SETTLE_SECONDS`. **Not yet the Phase D subgraph** — it renders the whole vault
    focused on the seed, because there is no expansion in the retrieval path to render
    yet. `yalive::graph::expand` is the function that will supply it.

**What this cost, and what it removed.** §7.2's three options for text resolved to (c):
`glyphon` is gone, labels are iced `text` widgets stacked over the shader, and they are
tiered by zoom so a full-vault view does not ask for thousands of widgets. §5's open
question "does `yGraphy` standalone also become iced" resolved to **yes**, and it was the
cheaper answer rather than the more ambitious one — migrating the winit/wgpu-23 surface
renderer to wgpu 27 was more work than deleting it, since iced owns the surface, the
device, the queue, and the event loop. Net: `winit`, `pollster`, and `glyphon` dropped
from `yGraphy`, one renderer, wgpu 27 everywhere, exactly as §7.3 wanted.

### Phase F — gateway escalation
31. `[llm.profiles.deep]` against OmniRoute's OpenAI-compatible `/v1`.
32. `Ctrl+Enter` as a distinct key. **No automatic fallback, ever** (§8.2).
33. Query-only by default; explicit, indicated opt-in to send context (§8.3).
34. `Save to note` → reviewed draft in `~/brain/inbox/`, `status: draft`, provenance
    frontmatter, and `outgoing::` relations back to the originating sections (§8.4).

### Phase G — only if D's benchmark demands it
35. Reconsider Stage 5 embeddings, with real evidence of what BM25 + graph missed.
    Two independent projects (Graphify, Semantica) now argue you will not need them.

---

## 5. Open questions

| Question | Decide at | How |
|---|---|---|
| ~~§2.2 — A, B, or C~~ | ~~Now~~ | ✅ **Decided: A.** `brain-index` wraps `yalive::db` / `yalive::parser`. |
| Does the graph beat lexical-only | Phase D | Recall@3 / MRR on the Phase B benchmark |
| Speculative decoding acceptance rate | Phase C | Measure tg with and without the draft model |
| Review state as a ranking signal — which direction | Phase D | Benchmark both; likely question-shape dependent |
| Is 200 output tokens enough | Phase C | Use it for a week |
| **Does `Mode::Hidden` preserve the XID in <50 ms** | **Phase A.7** | Spike. Fallback is `x11rb` map/unmap on the raw XID |
| ~~Graph labels~~ | ~~Phase A′~~ | ✅ **(c)**: `glyphon` dropped, labels are iced `text` widgets tiered by zoom. |
| ~~Does `yGraphy` standalone also become iced~~ | ~~Phase E~~ | ✅ **Yes**, and it was the cheaper path — deleting the surface renderer beat migrating it to wgpu 27. |
| Subchunk expansion — parent-resolve or inherit edges | Before Phase D | §6.4; parent-resolve is cheaper |
| Full bi-temporality, or just `supersedes` | After Phase D | Start with `supersedes`; extend only if you miss it |
| OmniRoute, or ~50 lines of `reqwest` | Phase F | Buy it for quota juggling, not by default |
| Embeddings at all | Phase G | Only with evidence from D |

---

## 6. Ideas worth taking from Semantica

[Semantica](https://github.com/semantica-agi/semantica) is Python, enterprise-scale, and
aimed at regulated-industry audit trails: RDF triple stores, SPARQL, SHACL/OWL governance,
Neo4j, Datalog, Rete networks, W3C PROV-O, six vector-store backends. **Roughly none of
that stack belongs here** — importing any of it into a three-Rust-binary personal vault
would be the exact failure `brain-dock-spec.md` §69 warns about.

But it independently reaches the same conclusion Graphify did — *a queryable graph beats
an embedding index for explainable retrieval* — which is now **two independent data points
for ordering Phase D before Phase F.** And five of its ideas transfer cleanly and cheaply.

### 6.1 Bi-temporal facts — the best idea here

Semantica tracks **valid time** (when a fact was true) separately from **recorded time**
(when it was learned). For a personal vault that accumulates five years of how-tos, this is
the difference between a useful assistant and one that confidently recites a workflow you
abandoned in 2023.

You are already halfway there and probably have not noticed: `brain-dock-spec.md` §12/§47
defines a `supersedes` frontmatter field, and `config/brain.example.toml` already carries
`[search.status_weight] obsolete = 0.25`. Both exist; neither is wired to the graph.

**Make `supersedes` a relation type.** It joins `related | outgoing | ingoing | contradicts
| example-of` in `relations.relation_type`, and then everything in §2.3 handles it for
free — graph expansion demotes the superseded section and promotes the superseder, without
a single new code path. That is roughly ten lines for the highest-value idea in this
section.

Full bi-temporality (nullable `valid_from` / `valid_until` on `sections`, alongside the
existing `created_at` / `modified_at`) is a later, optional refinement. It buys "how did I
*used to* do X" as a distinct answerable question. Do it only if you find yourself wanting
that.

### 6.2 Conflict detection before merge

Semantica flags contradictory facts *before* they enter the graph rather than silently
storing both. You already have both halves of the machinery: a `diagnostics(path, line,
message)` table, and broken-link detection at `src/db.rs:214`.

Extend it at index time: find **contradiction clusters** — pairs joined by `contradicts::`
where neither side is marked `obsolete`, `archived`, or superseded. That is an unresolved
disagreement with yourself, and it is precisely the thing that will make the dock give a
confidently wrong answer. Surface it in `brainctl doctor` as vault health.

Bonus: an unresolved contradiction is an excellent flashcard prompt. That is a `yReviewy`
feature falling out of a retrieval fix.

### 6.3 Provenance rows — which quietly solves the benchmark problem

Semantica's "decisions as first-class graph nodes" is compliance theatre at your scale.
The useful slice is one table: **every answer records the query, the `section_uid`s that
were packed into the prompt, the model, and the timestamp.**

Two payoffs, and the second is the real one:

- Stage 6 (corrections) needs exactly this table to know *what* it is correcting.
- **It generates the Phase B benchmark for free.** The plan currently calls for
  hand-labelling 30–50 questions with known-correct sections. Instead: log real queries
  with the sections used, add one keystroke in the dock to mark an answer good or bad, and
  after two weeks of normal use you have a labelled retrieval set built from *your actual
  questions* — which is strictly better data than anything you would sit down and invent.

This turns the most tedious blocker in the plan into a side effect of using the tool.

### 6.4 Entity-aware chunking — a bug you are otherwise going to ship

Semantica's GraphRAG-native splitter preserves entity and relation-triplet boundaries
across chunks. `yy` Stage 1 §1.4 splits oversized sections into ~450-token subchunks on
paragraph boundaries with ~60 tokens of overlap — and says nothing about what happens to
the section's relations.

The failure: a 900-token section with all its `[[links]]` in the first paragraph splits
into three subchunks. Subchunk 3 gets retrieved. Graph expansion from it finds **nothing**,
because every edge landed on subchunk 1. Graph retrieval silently degrades to lexical for
exactly the long, link-dense sections where it should help most.

Fix, in order of preference: (a) graph expansion always resolves a subchunk to its parent
section before traversing — one join, no schema change; or (b) subchunks inherit the
parent's relation set. Decide before Phase D, not after.

### 6.5 Retrieval explainability

Semantica's "decision-aware retrieval explains *why* results were selected." One line of
UI under the source badge:

```
OBS > Cursor follow > Smoothing        matched heading · 1 hop from current note
```

Cheap, and it is the debugger for Phase D. When graph retrieval returns the wrong section
you can see whether the seed was wrong or the expansion was.

### 6.6 Explicitly do not take

RDF, SPARQL, OWL, SHACL, Neo4j, Rete, Datalog, polyglot storage abstractions, LiteLLM,
audit-trail export. Also note their headline "6,000× faster node search (24 ms → 0.004 ms)"
on a 118k-node graph: that is what adding an index looks like. It is §3.2's finding with a
bigger number attached, not a new technique.

---

## 7. The graph panel under the chat area

Wanted: press a key, `yGraphy` appears below the dock's chat area in the same window; press
it again, it is gone. No separate app.

This is worth doing, but the reason it is worth doing is not convenience. **The panel should
show the subgraph that Phase D packed into the prompt** — the seed section and its typed
neighbours. That makes it a user-facing feature *and* the retrieval debugger from §6.5, out
of one implementation. A generic "here is my whole vault as dots" panel is much less useful
than "here is the neighbourhood of the answer you are reading."

### 7.1 With iced, this stops being an embedding problem

Under Slint this section was a menu of workarounds — texture import behind an unstable
feature, or X11 reparenting, or two windows pretending to be one. On iced it is just a
widget.

**`iced::widget::shader`** (stable, behind the default `wgpu` feature) hosts custom wgpu
inside a normal widget, laid out by the normal layout engine. The `Primitive` trait is a
close fit for what `yGraphy` already is:

| `Primitive` method | Receives | `yGraphy`'s equivalent today |
|---|---|---|
| associated type `Pipeline` | — | `Renderer`: pipelines, instance buffers, bind groups |
| `prepare()` | `&Device`, `&Queue`, bounds, viewport | camera uniform + instance buffer uploads |
| `draw()` | `&mut RenderPass` | the circle and line instanced draws |
| `render()` | `&mut CommandEncoder`, `&TextureView`, clip bounds | full control when `draw()` is not enough |

`render()` hands over the **raw `CommandEncoder` and a target `TextureView`**, so a
multi-pass renderer needs no restructuring at all. This corrects an earlier claim in
`IDEAS.md` that only egui exposes a raw encoder — iced exposes one too.

So the panel is a `shader` widget sitting below the answer area in the same view, shown or
hidden by a boolean in the dock's state. No reparenting, no texture marshalling, no second
process, no unstable features.

### 7.2 The one real migration cost: text

Version alignment for `iced_wgpu` 0.14:

```
iced 0.14  →  wgpu 27.0,  winit 0.30,  raw-window-handle 0.6
yGraphy    →  wgpu 23  →  must move to 27   (winit 0.30 already matches)
```

**`glyphon` has no wgpu-27 release.** The versions bracket it:

```
glyphon 0.9  → wgpu 25
glyphon 0.10 → wgpu 28      ← nothing targets 27
glyphon 0.11 → wgpu 29
glyphon 0.12 → wgpu 30
```

`yGraphy` is on `glyphon 0.7` (wgpu 23-era), so its text layer cannot come along as-is.
Three ways out, and the third is probably right:

- **(a) Patch `glyphon`.** It is a thin wrapper over `cosmic-text` + wgpu; a version bump is
  usually mechanical. Costs a vendored fork to maintain.
- **(b) Wait or pin.** Hold `iced` and `glyphon` at whatever pair does line up. Constrains
  both dependencies forever.
- **(c) Drop `glyphon`; render labels as iced `text` widgets** in a `stack!` over the
  shader widget, positioned by projecting graph coordinates through the camera. The panel
  shows a **subgraph** — a seed plus its neighbours, tens of nodes — so this is trivially
  cheap, and the labels inherit iced's font rendering, theming, and crisp text for free.

(c) removes a dependency and improves the result at panel scale. Its only weakness is
`yGraphy`'s **full-vault** mode, where thousands of labels would be too many iced widgets —
which is manageable by drawing labels only above a zoom threshold and only for visible
nodes, something a full-vault view wants regardless. **This is an open question worth
deciding deliberately** (§5).

### 7.3 What `yGraphy` becomes

Since the panel is now a widget rather than a foreign window, the cleanest end state is
that `yGraphy` and the dock share one renderer:

- `yalive::graph` — adjacency, degree/PageRank, k-hop expansion, communities (Phase A.5).
- `ygraphy` as a **library** — force layout plus a `shader::Program` implementation.
- The dock embeds it scoped to a subgraph; `yGraphy`'s binary is the same widget in its own
  iced window, scoped to the whole vault.

One renderer, one stack, wgpu 27 everywhere, no glyphon.

### 7.4 Requirements that do not change

- **The daemon owns visibility.** `brainctl graph toggle` → daemon flips `graph_visible` →
  the dock shows the panel. `brainctl` stays stateless. Unchanged by the port.
- **Render on demand.** `yGraphy` today runs its force simulation and redraws forever.
  Inside a window resident from login that is a permanent GPU cost. Redraw on a dirty flag,
  only while the panel is visible, and **let the simulation settle and stop.** iced is
  event-driven by default, so this is now the natural behaviour rather than a fight — but
  the force simulation still has to be told to converge.
- **Seed contextually.** The panel opens centred on the current primary source's
  `section_uid` and renders **the subgraph Phase D packed into the prompt** — not the whole
  vault. That is what makes it a feature and the retrieval debugger from one implementation.
- **Window resize.** Showing the panel grows the window downward; the top-right anchor must
  not move. Re-`ConfigureWindow` on height change without touching `x`/`y`.

Sources: [iced::widget::shader](https://docs.rs/iced/latest/iced/widget/shader/index.html) ·
[shader::Primitive](https://docs.rs/iced_widget/0.14.0/iced_widget/shader/trait.Primitive.html) ·
[iced::window::Mode](https://docs.rs/iced/latest/iced/window/enum.Mode.html) ·
[iced::daemon](https://docs.rs/iced/latest/iced/fn.daemon.html)

---

## 8. Free gateways for higher-tier answers

Wanted: use a stronger model than local Qwen3-1.7B for questions your notes cannot answer,
via free tiers, and save the result into the vault.
[OmniRoute](https://github.com/diegosouzapw/OmniRoute) aggregates 291+ providers (90+ free)
behind one **OpenAI-compatible `/v1` endpoint** on `localhost:20128`, with circuit
breakers, per-model 429 lockout, and quota-aware routing.

### 8.1 The integration is nearly free

`config/brain.example.toml` already has `backend`, `host`, `port`, and
`[llm.profiles.fast]`. Adding a profile pointed at an OpenAI-compatible local port is a
small, well-shaped change:

```toml
[llm.profiles.deep]
backend = "openai"
host    = "127.0.0.1"
port    = 20128
model   = "..."
```

The integration is the easy part. The next two subsections are the parts that matter.

### 8.2 The rule that must not be broken

**The gateway is a different key, never a fallback.**

- `Enter` → local Qwen3-1.7B. Private, offline, ~600 ms, answers **from your notes**.
- `Ctrl+Enter` → gateway. Networked, not private, seconds of latency, answers **from world
  knowledge**.

An automatic "local model was unsure, so escalate" fallback means a question about your
private notes silently ships your note contents to a third party you did not choose, at a
moment you were not thinking about it. **Never wire that.** Escalation is a keystroke, and
it is always deliberate.

Your config already gets this right — `[answers] general_knowledge_fallback = false`. Keep
that default and make the setting mean "may I answer from world knowledge at all", not
"may I escalate automatically".

### 8.3 Privacy is the actual design problem

Free tiers are free because your traffic has value. Design for that rather than around it:

- **Default: send the query only.** Never retrieved note bodies. This matches the stated
  use case — pulling in external knowledge, not analysing your vault.
- Sending retrieved context must be a **separate, explicit** action with a visible
  indicator in the dock while it is armed.
- A preflight that shows exactly what bytes are about to leave, once, the first time.
- `[logging] log_queries` and `log_source_paths` already exist; give the gateway path its
  own switches rather than reusing them.

### 8.4 Saving into the vault — where the invariants bite

This is the interesting half. The spec's invariant — *the LLM never creates an action* —
is about model output never becoming trusted structural data unreviewed. Writing files
from model output sits in the same family, so:

- **Human-confirmed, never automatic.** The dock shows the answer with a `Save to note`
  action; activating it writes a **draft** and opens it in nvim. You review before it is
  part of the vault.
- **Write a real yalive note, not a blob.** Frontmatter carrying `status: draft`,
  `source: omniroute/<model>`, `retrieved_at: <date>`; a proper heading so the section gets
  a `section_uid`; and — the important part — **relations back to the sections that
  prompted the question**: `outgoing:: [[origin-note#section]]`. Imported knowledge lands
  *inside the graph* on day one, so Phase D expansion reaches it immediately.
- That is §6.3's provenance idea arriving for a concrete reason: every imported note
  carries which model said it and when.
- `status: draft` already scores `× 0.9` in `[search.status_weight]`. So imported knowledge
  automatically ranks just below your own writing until you edit it. **That is exactly the
  right default and it costs nothing** — the machinery is already in the config file.
- Land drafts in a quarantine directory (`~/brain/inbox/`) so what is yours and what is
  imported is obvious at a glance.
- Loop closer: a saved note can carry a `quiz` block, which means `yReviewy` will make you
  actually learn the thing you asked about, instead of re-asking it in four months.

### 8.5 Honest costs

- **A Node daemon in a Rust project.** OmniRoute is Node/TS, ~100–200 MB resident. Run it
  as a systemd user unit — you already decided on systemd for `brain-daemon`
  (`09-decisions.md` #10) — or start it on demand and accept a few seconds on first deep
  query. Given deep queries are rare and deliberate, on-demand is defensible.
- **You will use about 2% of it.** 19 routing strategies, 105 MCP tools, 43-language UI.
  That is fine as long as you treat it as an opaque proxy and never integrate against its
  internals.
- **Turn token compression off.** Its 12-engine pipeline trades fidelity for tokens. On a
  free tier you are nowhere near the cap, so you would be degrading answer quality to save
  something you have a surplus of.
- **Free-tier lists rot.** Providers appear and vanish. Pin the version, expect breakage,
  and make sure a dead gateway degrades to a working local answer with no user-visible
  error — the same rule §51 already sets for the embedder.
- **You may not need it.** If two providers would do, calling them directly is ~50 lines of
  `reqwest` (already a `yReviewy` dependency) with no Node process. OmniRoute earns its
  place specifically for **quota juggling across many flaky free tiers** — which, given how
  free tiers behave, is a real problem worth outsourcing. Just buy it for that reason
  rather than by default.

---

The framing in `brain-dock-spec.md` §69 still governs, and none of the above changes it:

```
shortcut → ask → correct source → short useful answer → one-keystroke jump
```

The graph is worth building because it makes the *correct source* step better and the
*short useful answer* step cheaper. Nothing here is worth doing if it does not.
