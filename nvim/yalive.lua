return {
  dir = vim.fn.stdpath("config") .. "/lua/nabil/myplugin",
  name = "yalive.nvim",
  ft = "markdown",
  opts = {
    executable = vim.fn.expand("~/Dev/projects/yalive/target/debug/yalive"),
    auto_index = true,
    diagnostics = true,
  },
  keys = {
    { "<leader>yc", "<cmd>YaliveCard<cr>", desc = "Yalive card" },
    { "<leader>ys", "<cmd>YaliveSearch<cr>", desc = "Yalive search" },
    { "<leader>yo", "<cmd>YaliveLink<cr>", desc = "Yalive outgoing link" },
    { "<leader>yi", "<cmd>YaliveBacklink<cr>", desc = "Yalive incoming link" },
    { "<leader>yr", "<cmd>YaliveRelations<cr>", desc = "Yalive relations" },
    { "<leader>yx", "<cmd>YaliveIndex<cr>", desc = "Yalive index" },
    { "<leader>yd", "<cmd>YaliveDiagnostics<cr>", desc = "Yalive diagnostics" },
  },
}
