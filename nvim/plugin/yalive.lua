if vim.g.loaded_yalive_nvim then
  return
end
vim.g.loaded_yalive_nvim = true

require("yalive").setup()
