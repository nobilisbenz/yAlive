-- A lazy.nvim spec for this plugin, usable straight from a checkout.
--
-- Drop this file into your `lua/plugins/` directory (or `require` it from your
-- own spec list). It locates the plugin relative to itself, so the checkout can
-- live anywhere; the previous version hardcoded a path inside one particular
-- machine's Neovim config and could not load for anyone else.
local here = debug.getinfo(1, "S").source:sub(2)
local plugin_dir = vim.fn.fnamemodify(here, ":p:h")
local repo_root = vim.fn.fnamemodify(plugin_dir, ":h")

-- Prefer an installed `yalive`; fall back to a local cargo build so the plugin
-- works while developing without `cargo install` after every change.
local function executable()
  if vim.fn.executable("yalive") == 1 then
    return "yalive"
  end
  for _, profile in ipairs({ "release", "debug" }) do
    local candidate = repo_root .. "/target/" .. profile .. "/yalive"
    if vim.fn.executable(candidate) == 1 then
      return candidate
    end
  end
  return "yalive"
end

return {
  dir = plugin_dir,
  name = "yalive.nvim",
  ft = "markdown",
  opts = {
    executable = executable(),
    auto_index = true,
    diagnostics = true,
  },
  keys = {
    { "<leader>yc", "<cmd>YaliveCard<cr>", desc = "Yalive card" },
    { "<leader>ys", "<cmd>YaliveSearch<cr>", desc = "Yalive search" },
    { "<leader>yo", "<cmd>YaliveOutgoingLink<cr>", desc = "Yalive outgoing link" },
    { "<leader>yi", "<cmd>YaliveIngoingLink<cr>", desc = "Yalive ingoing link" },
    { "<leader>yr", "<cmd>YaliveRelations<cr>", desc = "Yalive relations" },
    { "<leader>yx", "<cmd>YaliveIndex<cr>", desc = "Yalive index" },
    { "<leader>yd", "<cmd>YaliveDiagnostics<cr>", desc = "Yalive diagnostics" },
    { "<leader>yv", "<cmd>YaliveVideos<cr>", desc = "Yalive videos" },
    { "<leader>yp", "<cmd>YalivePlay<cr>", desc = "Yalive play video" },
    { "<leader>yl", "<cmd>YaliveLibrary<cr>", desc = "Yalive clip library" },
  },
}
