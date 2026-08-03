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
    { "<leader>yo", "<cmd>YaliveLink<cr>", desc = "Yalive outgoing link" },
    { "<leader>yi", "<cmd>YaliveBacklink<cr>", desc = "Yalive incoming link" },
    { "<leader>yr", "<cmd>YaliveRelations<cr>", desc = "Yalive relations" },
  },
}
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
| `:YaliveRelations` | Browse both outgoing relations and incoming backlinks |
| `:YaliveIndex` | Reindex the vault |
| `:YaliveDiagnostics` | Publish parser and broken-link diagnostics for the current buffer |

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
