# yalive.nvim

Neovim integration for yalive vaults. Markdown remains the source of truth; the plugin communicates with the `yalive` executable through its versioned JSON editor commands.

## Requirements

- Neovim 0.10 or newer
- `yalive` installed in `$PATH`, for example with `cargo install --path /path/to/yalive`
- Optional: [Telescope](https://github.com/nvim-telescope/telescope.nvim), [`fzf-lua`](https://github.com/ibhagwan/fzf-lua), or [`fzf.vim`](https://github.com/junegunn/fzf.vim)

Pickers prefer Telescope, then `fzf-lua`, then `fzf.vim`, and otherwise use `vim.ui.select`.

## Install

With lazy.nvim, point at the plugin's `nvim` directory:

```lua
{
  dir = "/path/to/yalive/nvim",
  name = "yalive.nvim",
  ft = "markdown",
  opts = {
    executable = "yalive",
    auto_index = true,
    diagnostics = true,
    -- vault = "/path/to/vault", -- normally discovered from .notes/
  },
  keys = {
    { "<leader>yc", "<cmd>YaliveCard<cr>", desc = "Yalive card" },
    { "<leader>ys", "<cmd>YaliveSearch<cr>", desc = "Yalive sections" },
    { "<leader>yo", "<cmd>YaliveOutgoingLink<cr>", desc = "Yalive outgoing link" },
    { "<leader>yi", "<cmd>YaliveIngoingLink<cr>", desc = "Yalive ingoing link" },
    { "<leader>yr", "<cmd>YaliveRelations<cr>", desc = "Yalive relations" },
    { "<leader>yv", "<cmd>YaliveVideos<cr>", desc = "Yalive videos" },
    { "<leader>yp", "<cmd>YalivePlay<cr>", desc = "Yalive play video" },
  },
}
```

The repository ships a ready-made spec at [`nvim/yalive.lua`](yalive.lua). It
locates the plugin relative to itself and prefers an installed `yalive`, falling
back to `target/release` or `target/debug` in the checkout, so it works without
editing paths:

```lua
-- lua/plugins/yalive.lua
return dofile("/path/to/yalive/nvim/yalive.lua")
```

For development without installing the binary:

```lua
require("yalive").setup({
  executable = "/path/to/yalive/target/debug/yalive",
})
```

## Commands

| Command | Action |
| --- | --- |
| `:YaliveCard` | Pick any advertised card type, request an ID, and insert its complete syntax |
| `:YaliveSearch [query]` | Search all notes and sections and jump to a result |
| `:YaliveLink` | Pick a target and relation type, then add an outgoing relation to the current section |
| `:YaliveBacklink` | Pick a source and relation type, open it, then add a relation back to the current section |
| `:YaliveOutgoingLink` | Pick a target and immediately add an `outgoing::` relation |
| `:YaliveIngoingLink` | Pick a target and immediately add an `ingoing::` relation |
| `:YaliveRelations` | Browse both outgoing relations and incoming backlinks |
| `:YaliveIndex` | Reindex the vault |
| `:YaliveDiagnostics` | Publish parser and broken-link diagnostics for the current buffer |
| `:YaliveVideos` | Browse every `@video` in the vault and play one |
| `:YalivePlay` | Play the `@video` on this line, the URL under the cursor, or the section's first clip |
| `:YaliveLibrary [query]` | Browse the yClippy library and play a video or clip |
| `:YaliveInsertClip [query]` | Insert an `@video` line for a clip chosen from the yClippy library |

The three video commands were previously named `:YClippyPlay`, `:YClippyLibrary`,
and `:YClippyInsert`. Those names still work as aliases, but the `Yalive` prefix
is the one to use — they are this plugin's commands, and yClippy is simply the
player they hand the clip to.

`:YaliveBacklink` changes to the selected source buffer because yalive stores only outgoing links in Markdown. The edit is intentionally left unsaved for review.

## Configuration

```lua
require("yalive").setup({
  executable = "yalive",
  vault = nil,          -- string, function(path), or nil for .notes discovery
  auto_index = true,    -- index Markdown after writing
  diagnostics = true,   -- refresh vim.diagnostic after writing
})
```

When diagnostics are enabled, their refresh also performs indexing, avoiding duplicate work on save.
