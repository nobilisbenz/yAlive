# The yalive design system

Four apps, four languages, one product. `yalive` draws with ratatui, `yGraphy`
with wgpu and iced, `yClippy` with Svelte and Tailwind, `yReviewy` with plain
CSS. They cannot share an import, so they share a **generator**.

```
assets/design/tokens.json     ← the only place a value is decided
assets/design/generate.py     ← writes the per-app files
        │
        ├── src/app/tokens.rs         (yalive)
        ├── yGraphy/src/tokens.rs     (yGraphy)
        ├── yClippy/src/tokens.css    (yClippy)
        ├── yReviewy/src/tokens.css   (yReviewy)
        └── assets/icons/*.svg        (all four icons)
```

Edit `tokens.json`, then:

```bash
python3 assets/design/generate.py           # write the files
python3 assets/design/generate.py --check   # what CI runs
```

The generated files are committed, so no build needs Python. CI regenerates and
fails if the committed output has gone stale — which is what stops four hand-kept
palettes from drifting apart, as they had.

## What is shared, and what is not

**Shared: colour, spacing, radii, motion, type stacks.** These are the things
that make two windows look like they came from the same place.

**Not shared: shape and density.** yReviewy is read one-handed at length and
keeps its serif headings, its one-soft-corner cards, and its thumb-sized
targets. yClippy is a dense desktop tool with 32-pixel rows. yalive is a
terminal. A design system unifies the palette; it does not make four apps into
the same app.

## The ground

Every app sits on the same near-black, with six steps from background to border:

| Token | Value | For |
| --- | --- | --- |
| `bg` / `GROUND` | `#08080a` | the page |
| `surface` | `#0f0f12` | a raised panel or card |
| `surface-hi` | `#1a1a1f` | hover |
| `surface-active` | `#212128` | pressed, or selected without the accent |
| `border` | `#1f1f24` | the default edge |
| `border-hi` | `#2a2a30` | an edge that needs to be seen |

The TUI is the exception, and deliberately: it inherits the terminal's own
background rather than painting `#08080a` over it. A terminal app that overrides
the colour scheme you chose is the one window that feels foreign.

## Text, in three weights

`text` `#f4f4f5`, `text-dim` `#a1a1aa`, `text-faint` `#52525b`. Three, and only
three — a fourth is a decision nobody can make consistently twice.

Again the TUI defers: body text is the terminal's own foreground, and only the
two supporting weights are tokens.

## One accent per app

Each app owns one hue, and it is the hue at the centre of its icon:

| App | Accent | |
| --- | --- | --- |
| yalive | `#168bff` | blue |
| yGraphy | `#ff3b30` | red |
| yReviewy | `#b7ff2a` | lime |
| yClippy | `#ff9f0a` | amber |

They are far enough apart to be told apart in a dock, a tab bar, or a window
list. Two of them used to be nearly the same blue, which is the one thing a
family of accents must not be.

Every app also gets *every* accent as a token (`--accent-yclippy`,
`ACCENT_YGRAPHY`, …), so one app can point at another and be believed —
yReviewy's play button is yClippy amber because yClippy is what answers the tap.

**Text on an accent comes from `--on-accent`**, never from a hardcoded `#fff`.
Lime and amber are light; white on either is unreadable. The generator computes
it from the accent's luma.

## Colour means something

Beyond the accent, only three colours carry meaning, and they mean the same
thing in all four apps:

`success` `#22c55e` · `warning` `#f59e0b` · `danger` `#ef4444`

Everything else on screen is ground, text, or accent. If a new colour is being
reached for, the question to ask first is what it would *mean*.

## Data is a separate palette

The graph tints one hue per note, which is categorical data, not emphasis. That
needs its own ramp — eight hues, cycled, chosen to stay legible on the dark
ground and, critically, to stay clear of every accent so a node is never
mistaken for a selection. `yGraphy/src/theme.rs` has a test that enforces the
distance.

Reach for `chart-1…8` for anything encoding a category. Never reach for an
accent: an accent means "this app" or "this is selected", and spending it on the
fourth item in a list destroys both meanings.

## Adding a value

1. Add it to `tokens.json`, in the group it belongs to.
2. If it needs a new shape in a generated file, teach `generate.py` to emit it.
3. Run the generator. Commit the output alongside the token.

If a value is only ever used by one app, it does not belong here — yClippy's
`--row-h` and `--titlebar-h` live in yClippy, because no other app has a
titlebar.
