-- arugula.nvim: your nvim in the Arugula swarm (M28). Options go in
-- vim.g.arugula (`{ socket = "/path/to/sock" }`) before this runs.
if vim.g.loaded_arugula then return end
vim.g.loaded_arugula = true

local arugula = require("arugula")
-- vim.g.illogical: set by configs from before the rename (#505).
arugula.setup(vim.g.arugula or vim.g.illogical or {})

vim.api.nvim_create_user_command("ArugulaJoin", arugula.join, { desc = "Show this folder in the Arugula swarm" })
vim.api.nvim_create_user_command("ArugulaLeave", arugula.leave, { desc = "Take this folder out of the Arugula swarm" })
vim.api.nvim_create_user_command("ArugulaStatus", function()
  local s = arugula.status()
  vim.notify(s == "" and "arugula: not in the swarm" or ("arugula: " .. s))
end, { desc = "Whether this nvim is in the swarm, and who follows it" })

-- The commands' names before the rename, for configs and muscle memory (#505).
vim.api.nvim_create_user_command("IllogicalJoin", "ArugulaJoin", { desc = "Old name of :ArugulaJoin (#505)" })
vim.api.nvim_create_user_command("IllogicalLeave", "ArugulaLeave", { desc = "Old name of :ArugulaLeave (#505)" })
vim.api.nvim_create_user_command("IllogicalStatus", "ArugulaStatus", { desc = "Old name of :ArugulaStatus (#505)" })
