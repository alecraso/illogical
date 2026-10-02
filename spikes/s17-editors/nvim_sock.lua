-- S17: can an nvim plugin reach illogicald's unix socket with nothing but core? (vim.uv pipes)
-- nvim --headless --clean -l nvim_sock.lua /tmp/s17-sock/nvim.sock
local path = arg[1]
local p = vim.uv.new_pipe(false)
local done = false
p:connect(path, function(err)
  if err then
    io.stderr:write("connect failed: " .. err .. "\n")
    done = true
    return
  end
  local hello = vim.json.encode({ ev = "hello", editor = "nvim", pid = vim.fn.getpid(), version = tostring(vim.version()) })
  p:write(hello .. "\n", function()
    p:close()
    done = true
  end)
end)
vim.wait(2000, function() return done end)
