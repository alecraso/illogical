-- illogical.nvim: your nvim in the illogical swarm (M28). Options go in
-- vim.g.illogical (`{ socket = "/path/to/sock" }`) before this runs.
if vim.g.loaded_illogical then return end
vim.g.loaded_illogical = true

local illogical = require("illogical")
illogical.setup(vim.g.illogical or {})

vim.api.nvim_create_user_command("IllogicalJoin", illogical.join, { desc = "Show this folder in the illogical swarm" })
vim.api.nvim_create_user_command("IllogicalLeave", illogical.leave, { desc = "Take this folder out of the illogical swarm" })
vim.api.nvim_create_user_command("IllogicalStatus", function()
  local s = illogical.status()
  vim.notify(s == "" and "illogical: not in the swarm" or ("illogical: " .. s))
end, { desc = "Whether this nvim is in the swarm, and who follows it" })
