-- arugula.nvim (M28): your nvim in the Arugula swarm.
--
-- It connects to the arugulad on this machine ($ARUGULA_SOCK, else the
-- daemon's usual socket) with core vim.uv, no dependencies, and speaks the
-- daemon's editor protocol (lines of JSON over an HTTP upgrade; see the
-- daemon's editor/link.rs), as the VS Code extension does:
--
--   summary   the file, diagnostic counts, unsaved buffers, the debugger
--             (nvim-dap, if it's there) and a merge conflict: at most once a
--             second, at once when something wants attention
--   peek      the lines around the cursor, for previews
--   follow, open, edit, diagnostics
--             only while someone follows (the daemon says `followers`):
--             the cursor at most every 100 ms, the file, each change
--
-- A folder joins only when asked (:ArugulaJoin), and that's remembered
-- for it; :ArugulaLeave takes it out at once.
--
-- Lines count from 1 and columns from 0 (UTF-16 units), as the follow
-- view (and VS Code) count them.

local M = {}

local uv = vim.uv or vim.loop
local SUMMARY_MS = 1000
local PEEK_MS = 1000
local FOLLOW_MS = 100
local MAX_TEXT = 1024 * 1024
local ABOVE, LINES = 3, 7

local S = {
  on = false,
  pipe = nil,
  id = nil,
  followers = 0,
  buf = "",
  timers = {},
  last = {},
  sent = { summary = {}, peek = "", follow = "" },
  open = nil, -- the buffer whose text followers have
  counts = {}, -- line counts of attached buffers
  attached = {},
  debug = vim.NIL,
  saved = false,
  folder = nil,
  retry = nil,
  backoff = 500,
  -- The upgrade's protocol name. A daemon from before the rename (0.25 and
  -- older) knows only the old one and refuses the new with a 400 (#505).
  proto = "arugula-editor",
}

local OLD_PROTO = "illogical-editor"

-- ---- where things are

local function state_dir(name)
  local base = vim.env.XDG_STATE_HOME or (vim.env.HOME .. "/.local/state")
  return base .. "/" .. name
end

local function env(name)
  local v = vim.env[name]
  if v and v ~= "" then return v end
end

local function socket_path()
  if M.opts.socket then return M.opts.socket end
  -- The old names too: a shell started before the update, a daemon whose
  -- state is still in the old directory (#505).
  local set = env("ARUGULA_SOCK") or env("ILLOGICAL_SOCK")
  if set then return set end
  for _, name in ipairs({ "arugula", "illogical" }) do
    local dir = state_dir(name)
    local f = io.open(dir .. "/sock.path", "r")
    if f then
      local p = vim.trim(f:read("*a") or "")
      f:close()
      if p ~= "" then return p end
    end
    if uv.fs_stat(dir .. "/sock") then return dir .. "/sock" end
  end
  return state_dir("arugula") .. "/sock"
end

-- Folders that joined, remembered.
local function remembered_path(name)
  return vim.fn.stdpath("data") .. "/" .. (name or "arugula") .. "/folders.json"
end

local function remembered()
  -- Where they were remembered before the rename, until one changes (#505).
  local f = io.open(remembered_path(), "r") or io.open(remembered_path("illogical"), "r")
  if not f then return {} end
  local ok, v = pcall(vim.json.decode, f:read("*a") or "")
  f:close()
  return ok and type(v) == "table" and v or {}
end

local function remember(folder, on)
  local list = remembered()
  local out = {}
  for _, d in ipairs(list) do
    if d ~= folder then table.insert(out, d) end
  end
  if on then table.insert(out, folder) end
  vim.fn.mkdir(vim.fn.fnamemodify(remembered_path(), ":h"), "p")
  local f = io.open(remembered_path(), "w")
  if f then
    f:write(vim.json.encode(out))
    f:close()
  end
end

-- ---- sending

local function send(msg)
  if not S.pipe or not S.id and msg.t ~= "hello" then return end
  S.pipe:write(vim.json.encode(msg) .. "\n")
end

-- At most once every `every` ms; never a trailing debounce (S17).
local function throttle(key, every, fn)
  if S.timers[key] then return end
  local wait = math.max(0, (S.last[key] or 0) + every - uv.now())
  local t = uv.new_timer()
  S.timers[key] = t
  t:start(wait, 0, vim.schedule_wrap(function()
    t:close()
    S.timers[key] = nil
    S.last[key] = uv.now()
    fn()
  end))
end

local function utf16(s)
  local ok, n = pcall(vim.str_utfindex, s, "utf-16")
  if ok and type(n) == "number" then return n end
  ok, n = pcall(vim.str_utfindex, s)
  if ok and type(n) == "number" then return n end
  return #s
end

-- A real file in a normal buffer.
local function file_of(buf)
  buf = buf or vim.api.nvim_get_current_buf()
  if vim.bo[buf].buftype ~= "" then return nil end
  local name = vim.api.nvim_buf_get_name(buf)
  if name == "" then return nil end
  return vim.fn.fnamemodify(name, ":p")
end

local function diag_counts()
  local c = { e = 0, w = 0, i = 0 }
  for _, d in ipairs(vim.diagnostic.get(nil)) do
    if d.severity == vim.diagnostic.severity.ERROR then
      c.e = c.e + 1
    elseif d.severity == vim.diagnostic.severity.WARN then
      c.w = c.w + 1
    else
      c.i = c.i + 1
    end
  end
  return c
end

local function dirty_count()
  local n = 0
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(b) and vim.bo[b].modified and file_of(b) then n = n + 1 end
  end
  return n
end

local function conflict()
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(b) and file_of(b) and vim.api.nvim_buf_line_count(b) < 50000 then
      local lines = vim.api.nvim_buf_get_lines(b, 0, -1, false)
      local a, m, z = false, false, false
      for _, l in ipairs(lines) do
        if l:sub(1, 8) == "<<<<<<< " then a = true
        elseif l == "=======" then m = true
        elseif l:sub(1, 8) == ">>>>>>> " then z = true end
      end
      if a and m and z then return file_of(b) end
    end
  end
  return vim.NIL
end

local function send_summary()
  local now = {
    file = file_of() or vim.NIL,
    diag = diag_counts(),
    dirty = dirty_count(),
    debug = S.debug,
    conflict = conflict(),
  }
  local out, any = { t = "summary" }, false
  for k, v in pairs(now) do
    if vim.json.encode(v) ~= vim.json.encode(S.sent.summary[k] == nil and "unset" or S.sent.summary[k]) then
      out[k] = v
      any = true
    end
  end
  if S.saved then
    out.saved = true
    S.saved = false
    any = true
  end
  if not any then return end
  S.sent.summary = now
  send(out)
end

local function send_peek()
  local file = file_of()
  local msg
  if not file then
    msg = { t = "peek", file = vim.NIL, dirty = dirty_count() }
  else
    local buf = vim.api.nvim_get_current_buf()
    local cur = vim.api.nvim_win_get_cursor(0)
    local count = vim.api.nvim_buf_line_count(buf)
    local top = math.max(0, math.min(cur[1] - 1 - ABOVE, count - LINES))
    local lines = vim.api.nvim_buf_get_lines(buf, top, math.min(count, top + LINES), false)
    for i, l in ipairs(lines) do lines[i] = l:sub(1, 240) end
    msg = { t = "peek", file = file, line = cur[1], col = cur[2] + 1, top = top + 1, lines = lines, dirty = dirty_count() }
  end
  local s = vim.json.encode(msg)
  if s == S.sent.peek then return end
  S.sent.peek = s
  send(msg)
end

local SEVERITY = { "error", "warning", "info", "hint" }

local function send_diagnostics(buf)
  local items = {}
  for _, d in ipairs(vim.diagnostic.get(buf)) do
    local line = vim.api.nvim_buf_get_lines(buf, d.lnum, d.lnum + 1, false)[1] or ""
    local eline = vim.api.nvim_buf_get_lines(buf, d.end_lnum or d.lnum, (d.end_lnum or d.lnum) + 1, false)[1] or ""
    table.insert(items, {
      range = { d.lnum + 1, utf16(line:sub(1, d.col)), (d.end_lnum or d.lnum) + 1, utf16(eline:sub(1, d.end_col or d.col)) },
      severity = SEVERITY[d.severity] or "info",
      message = (d.message or ""):sub(1, 500),
    })
  end
  send({ t = "diagnostics", file = file_of(buf), items = items })
end

local function on_lines(_, buf, _, first, last, new_last)
  if not S.on or S.followers == 0 or S.open ~= buf then
    S.counts[buf] = nil
    return S.on ~= true -- detach once off
  end
  local old = S.counts[buf] or vim.api.nvim_buf_line_count(buf)
  local new = vim.api.nvim_buf_get_lines(buf, first, new_last, false)
  local range, text
  if last < old then
    range = { first + 1, 0, last + 1, 0 }
    text = #new > 0 and (table.concat(new, "\n") .. "\n") or ""
  elseif #new > 0 then
    -- Up to the end of the file, which has no newline after its last line.
    if first < old then
      range = { first + 1, 0, old + 1, 0 }
      text = table.concat(new, "\n")
    else
      range = { old + 1, 0, old + 1, 0 }
      text = "\n" .. table.concat(new, "\n")
    end
  elseif first > 0 then
    local prev = vim.api.nvim_buf_get_lines(buf, first - 1, first, false)[1] or ""
    range = { first, utf16(prev), old + 1, 0 }
    text = ""
  else
    range = { 1, 0, old + 1, 0 }
    text = ""
  end
  S.counts[buf] = old - (last - first) + (new_last - first)
  send({ t = "edit", file = file_of(buf), version = vim.b[buf].changedtick, changes = { { range = range, text = text } } })
end

local function send_open(buf)
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
  local text = table.concat(lines, "\n")
  local big = #text > MAX_TEXT
  S.open = buf
  S.counts[buf] = #lines
  if not S.attached[buf] then
    S.attached[buf] = true
    vim.api.nvim_buf_attach(buf, false, {
      on_lines = on_lines,
      on_detach = function() S.attached[buf] = nil end,
    })
  end
  send({ t = "open", file = file_of(buf), version = vim.b[buf].changedtick, lang = vim.bo[buf].filetype, text = big and vim.NIL or text, too_big = big or nil })
  send_diagnostics(buf)
end

local function send_follow()
  if S.followers == 0 then return end
  local buf = vim.api.nvim_get_current_buf()
  local file = file_of(buf)
  if not file then return end
  if S.open ~= buf then send_open(buf) end
  local cur = vim.api.nvim_win_get_cursor(0)
  local line = vim.api.nvim_buf_get_lines(buf, cur[1] - 1, cur[1], false)[1] or ""
  local mode = vim.api.nvim_get_mode().mode
  local sel = vim.NIL
  if mode:match("^[vV\22]") then
    local v = vim.fn.getpos("v")
    local a, b = { v[2], v[3] - 1 }, { cur[1], cur[2] }
    if a[1] > b[1] or (a[1] == b[1] and a[2] > b[2]) then a, b = b, a end
    local la = vim.api.nvim_buf_get_lines(buf, a[1] - 1, a[1], false)[1] or ""
    local lb = vim.api.nvim_buf_get_lines(buf, b[1] - 1, b[1], false)[1] or ""
    sel = { a[1], utf16(la:sub(1, a[2])), b[1], utf16(lb:sub(1, b[2] + 1)) }
  end
  local msg = {
    t = "follow",
    file = file,
    line = cur[1],
    col = utf16(line:sub(1, cur[2])),
    sel = sel,
    view = { vim.fn.line("w0"), vim.fn.line("w$") },
    mode = mode:sub(1, 1),
  }
  local s = vim.json.encode(msg)
  if s == S.sent.follow then return end
  S.sent.follow = s
  send(msg)
end

local function follow_all()
  S.open = nil
  S.sent.follow = ""
  local buf = vim.api.nvim_get_current_buf()
  if file_of(buf) then
    send_open(buf)
    send_follow()
  end
end

-- What changed: send what it touches.
local function changed(what)
  if not S.id then return end
  if what.summary then
    if what.now then
      S.last.summary = uv.now()
      send_summary()
    else
      throttle("summary", SUMMARY_MS, send_summary)
    end
  end
  if what.peek then throttle("peek", PEEK_MS, send_peek) end
  if what.follow and S.followers > 0 then throttle("follow", FOLLOW_MS, send_follow) end
end

-- ---- the connection

local function on_message(m)
  if m.t == "welcome" then
    S.id = m.id
    S.backoff = 500
    S.sent = { summary = {}, peek = "", follow = "" }
    send_summary()
    send_peek()
  elseif m.t == "followers" then
    local more = m.n > S.followers
    S.followers = m.n
    if more then
      vim.notify(("arugula: %d following"):format(m.n))
      follow_all()
    elseif m.n == 0 then
      S.open = nil
    end
  elseif m.t == "resend" then
    follow_all()
  elseif m.t == "continue" then
    local ok, dap = pcall(require, "dap")
    if ok then dap.continue() end
  end
end

local connect

local function lost()
  if S.pipe then
    pcall(function() S.pipe:close() end)
  end
  S.pipe, S.id, S.followers, S.buf, S.open = nil, nil, 0, "", nil
  if S.on and not S.retry then
    S.retry = uv.new_timer()
    S.retry:start(S.backoff, 0, vim.schedule_wrap(function()
      S.retry:close()
      S.retry = nil
      connect()
    end))
    S.backoff = math.min(S.backoff * 2, 10000)
  end
end

connect = function()
  if S.pipe or not S.on then return end
  local pipe = uv.new_pipe(false)
  S.pipe = pipe
  local upgraded = false
  pipe:connect(socket_path(), function(err)
    if err then return vim.schedule(lost) end
    pipe:write("GET /api/editors/connect HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: " .. S.proto .. "\r\n\r\n")
    pipe:read_start(function(rerr, data)
      if rerr or not data then return vim.schedule(lost) end
      vim.schedule(function()
        if S.pipe ~= pipe then return end
        S.buf = S.buf .. data
        if not upgraded then
          local i = S.buf:find("\r\n\r\n", 1, true)
          if not i then return end
          if not S.buf:match("^HTTP/1.1 101") then
            -- Refused: the daemon may know the protocol by its other name.
            if S.buf:match("^HTTP/1.1 400") then
              S.proto = S.proto == OLD_PROTO and "arugula-editor" or OLD_PROTO
            end
            return lost()
          end
          upgraded = true
          S.buf = S.buf:sub(i + 4)
          local cur = file_of()
          send({
            t = "hello",
            editor = "nvim",
            hostname = uv.os_gethostname(),
            workspace = S.folder,
            remote = vim.env.SSH_CONNECTION and "ssh" or vim.NIL,
          })
          if cur then S.sent.peek = "" end
        end
        while true do
          local j = S.buf:find("\n", 1, true)
          if not j then break end
          local line = S.buf:sub(1, j - 1)
          S.buf = S.buf:sub(j + 1)
          if line ~= "" then
            local ok, m = pcall(vim.json.decode, line)
            if ok and type(m) == "table" then on_message(m) end
          end
        end
      end)
    end)
  end)
end

local function disconnect()
  S.on = false
  if S.retry then
    S.retry:stop()
    S.retry:close()
    S.retry = nil
  end
  if S.pipe then
    local p = S.pipe
    S.pipe = nil
    pcall(function() p:close() end)
  end
  S.id, S.followers, S.open = nil, 0, nil
end

-- ---- the debugger (nvim-dap, when it's there)

local function set_debug(d)
  if vim.json.encode(d) == vim.json.encode(S.debug) then return end
  S.debug = d
  changed({ summary = true, now = true })
end

local function hook_dap()
  local ok, dap = pcall(require, "dap")
  if not ok then return end
  dap.listeners.after.event_stopped["arugula"] = function(session, body)
    local frame = session.current_frame
    set_debug({
      state = "paused",
      reason = body and body.reason or vim.NIL,
      file = frame and frame.source and frame.source.path or vim.NIL,
      line = frame and frame.line or vim.NIL,
    })
    -- The frame comes a moment later.
    vim.defer_fn(function()
      local f = session.current_frame
      if f and S.debug ~= vim.NIL and S.debug.state == "paused" then
        set_debug({ state = "paused", reason = S.debug.reason, file = f.source and f.source.path or vim.NIL, line = f.line })
      end
    end, 300)
  end
  dap.listeners.after.event_continued["arugula"] = function() set_debug({ state = "running" }) end
  dap.listeners.after.event_terminated["arugula"] = function() set_debug(vim.NIL) end
  dap.listeners.after.event_exited["arugula"] = function() set_debug(vim.NIL) end
end

-- ---- public

M.opts = {}

--- Join the swarm with this folder (the current directory), and remember it.
function M.join()
  S.folder = vim.fn.getcwd()
  remember(S.folder, true)
  S.on = true
  connect()
  vim.notify("arugula: " .. S.folder .. " is in the swarm")
end

--- Leave at once, and forget the folder.
function M.leave()
  remember(S.folder or vim.fn.getcwd(), false)
  disconnect()
  vim.notify("arugula: out of the swarm")
end

--- For a statusline: "", "arugula", or "2 following".
function M.status()
  if not S.on then return "" end
  if S.followers > 0 then return ("%d following"):format(S.followers) end
  return S.id and "arugula" or "arugula…"
end

--- The pane id the daemon gave it, if connected.
function M.id()
  return S.id
end

function M.setup(opts)
  M.opts = opts or {}
  local g = vim.api.nvim_create_augroup("arugula", { clear = true })
  local au = function(ev, what) vim.api.nvim_create_autocmd(ev, { group = g, callback = function() changed(what) end }) end
  au({ "BufEnter" }, { summary = true, peek = true, follow = true })
  au({ "CursorMoved", "CursorMovedI", "ModeChanged" }, { peek = true, follow = true })
  au({ "WinScrolled" }, { follow = true })
  au({ "TextChanged", "TextChangedI" }, { peek = true })
  au({ "BufModifiedSet", "BufReadPost", "BufDelete" }, { summary = true })
  vim.api.nvim_create_autocmd("BufWritePost", {
    group = g,
    callback = function()
      S.saved = true
      changed({ summary = true, peek = true, now = true })
    end,
  })
  vim.api.nvim_create_autocmd("DiagnosticChanged", {
    group = g,
    callback = function(a)
      changed({ summary = true })
      if S.followers > 0 and a.buf == S.open then send_diagnostics(a.buf) end
    end,
  })
  vim.api.nvim_create_autocmd("VimLeavePre", { group = g, callback = disconnect })
  hook_dap()
  -- A folder that joined before joins again.
  local cwd = vim.fn.getcwd()
  for _, d in ipairs(remembered()) do
    if d == cwd then
      S.folder = cwd
      S.on = true
      connect()
      break
    end
  end
end

return M
