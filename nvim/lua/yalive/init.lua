local M = {}

local config = {
  executable = "yalive",
  -- The video surface. `player` is the argv template used to launch a moment;
  -- it shares its placeholder shape with yalive's `.notes/config.toml` and yy's
  -- `[openers]`, so one mental model covers all three.
  clipper = "yclippy",
  -- Leave `player` unset to get the shared resolution chain below. Set it to an
  -- argv template to pin one player.
  player = nil,
  vault = nil,
  auto_index = true,
  diagnostics = true,
}

local namespace = vim.api.nvim_create_namespace("yalive")
local commands_created = false

local function notify(message, level)
  vim.notify(message, level or vim.log.levels.INFO, { title = "yalive" })
end

local function vault_for(path)
  if type(config.vault) == "function" then
    return config.vault(path)
  end
  if type(config.vault) == "string" then
    return vim.fs.normalize(config.vault)
  end
  path = path ~= "" and path or vim.uv.cwd()
  local stat = vim.uv.fs_stat(path)
  if stat and stat.type ~= "directory" then
    path = vim.fs.dirname(path)
  end
  local marker = vim.fs.find(".notes", { path = path, upward = true, type = "directory" })[1]
  return marker and vim.fs.dirname(marker) or nil
end

local function current_vault()
  local vault = vault_for(vim.api.nvim_buf_get_name(0))
  if not vault then
    notify("Current buffer is not inside a yalive vault", vim.log.levels.ERROR)
  end
  return vault
end

local function command(vault, args)
  if vim.fn.executable(config.executable) ~= 1 then
    notify("Executable not found: " .. config.executable, vim.log.levels.ERROR)
    return nil
  end

  local argv = { config.executable, "--vault", vault }
  vim.list_extend(argv, args)
  local ok, process = pcall(vim.system, argv, { text = true })
  if not ok then
    notify("Failed to start " .. config.executable .. ": " .. tostring(process), vim.log.levels.ERROR)
    return nil
  end
  local result = process:wait()
  if result.code ~= 0 then
    notify(vim.trim(result.stderr ~= "" and result.stderr or result.stdout), vim.log.levels.ERROR)
    return nil
  end
  return result.stdout
end

local function editor(vault, args)
  local full_args = { "editor" }
  vim.list_extend(full_args, args)
  local output = command(vault, full_args)
  if not output then
    return nil
  end
  local ok, value = pcall(vim.json.decode, output)
  if not ok or value.protocol_version ~= 1 then
    notify("Unsupported or invalid yalive editor response", vim.log.levels.ERROR)
    return nil
  end
  return value
end

local function pick(items, label, format, callback)
  if #items == 0 then
    notify("No " .. label:lower() .. " found")
    return
  end

  local telescope_ok, pickers = pcall(require, "telescope.pickers")
  if telescope_ok then
    local finders = require("telescope.finders")
    local actions = require("telescope.actions")
    local action_state = require("telescope.actions.state")
    local conf = require("telescope.config").values
    pickers.new({}, {
      prompt_title = label,
      finder = finders.new_table({
        results = items,
        entry_maker = function(item)
          local text = format(item)
          return { value = item, display = text, ordinal = text }
        end,
      }),
      sorter = conf.generic_sorter({}),
      attach_mappings = function(prompt_bufnr)
        actions.select_default:replace(function()
          local selection = action_state.get_selected_entry()
          actions.close(prompt_bufnr)
          if selection then
            vim.schedule(function() callback(selection.value) end)
          end
        end)
        return true
      end,
    }):find()
    return
  end

  local labels, lookup = {}, {}
  for index, item in ipairs(items) do
    local text = format(item)
    if lookup[text] then
      text = text .. "  [" .. index .. "]"
    end
    labels[index], lookup[text] = text, item
  end

  local ok, fzf = pcall(require, "fzf-lua")
  if ok then
    fzf.fzf_exec(labels, {
      prompt = label .. "> ",
      actions = {
        ["default"] = function(selected)
          if selected and selected[1] then
            callback(lookup[selected[1]])
          end
        end,
      },
    })
    return
  end

  if vim.fn.exists("*fzf#run") == 1 then
    vim.fn["fzf#run"](vim.fn["fzf#wrap"]({
      source = labels,
      options = { "--prompt", label .. "> " },
      sink = function(selected)
        callback(lookup[selected])
      end,
    }))
    return
  end

  vim.ui.select(items, { prompt = label .. ":", format_item = format }, function(item)
    if item then
      callback(item)
    end
  end)
end

local function sections(vault, query)
  local response = editor(vault, { "sections", query or "" })
  return response and response.items or nil
end

local function section_label(section)
  return string.format("%s > %s  [%s]", section.note_title, section.heading, section.uid)
end

local function open_section(vault, section)
  vim.cmd.edit(vim.fn.fnameescape(vim.fs.joinpath(vault, section.path)))
  vim.api.nvim_win_set_cursor(0, { math.max(section.start_line, 1), 0 })
end

local function relative_path(vault, path)
  local normalized = vim.fs.normalize(path)
  local prefix = vim.fs.normalize(vault) .. "/"
  return normalized:sub(1, #prefix) == prefix and normalized:sub(#prefix + 1) or normalized
end

local function current_section(vault, all_sections)
  local path = relative_path(vault, vim.api.nvim_buf_get_name(0))
  local line = vim.api.nvim_win_get_cursor(0)[1]
  local best
  for _, section in ipairs(all_sections) do
    if vim.fs.normalize(section.path) == path and section.start_line <= line then
      if not best or section.start_line > best.start_line then
        best = section
      end
    end
  end
  if not best then
    notify("Save and index this note before creating a relation", vim.log.levels.ERROR)
  end
  return best
end

local function insert_line(text)
  vim.api.nvim_put({ text }, "l", true, true)
end

-- ── video ──────────────────────────────────────────────────────────────────

local function format_hms(seconds)
  seconds = math.max(0, math.floor(tonumber(seconds) or 0))
  local h = math.floor(seconds / 3600)
  local m = math.floor((seconds % 3600) / 60)
  local s = seconds % 60
  if h > 0 then
    return string.format("%d:%02d:%02d", h, m, s)
  end
  return string.format("%d:%02d", m, s)
end

-- Expands `{url}` and `{seconds}` per element, never re-splitting on
-- whitespace. A template without `{seconds}` gets the timestamp rebuilt into
-- the URL, so `{ "xdg-open", "{url}" }` still lands at the right moment.
--- Resolve the player: configured → yclippy → mpv → xdg-open.
---
--- The same chain `src/player.rs` and yy's `[openers]` follow, so one `@video`
--- line opens the same way from the TUI, the daemon, and here. This plugin used
--- to hardcode yclippy while the TUI defaulted to `xdg-open`, so the same line
--- behaved differently depending on which surface you pressed the key in.
---
--- A configured player that is not installed falls back silently: a config
--- synced from another machine should still open the video.
local function resolve_player()
  local candidates = {}
  if config.player and #config.player > 0 then
    table.insert(candidates, config.player)
  end
  table.insert(candidates, { config.clipper or "yclippy", "play", "{url}", "--at", "{seconds}" })
  table.insert(candidates, { "mpv", "--start={seconds}", "{url}" })
  table.insert(candidates, { "xdg-open", "{url}" })
  for _, template in ipairs(candidates) do
    if vim.fn.executable(template[1]) == 1 then
      return template
    end
  end
  return candidates[#candidates]
end

local function expand_player(url, seconds, override)
  local template = override or config.player
  if type(template) == "string" then
    template = { template, "{url}" }
  end

  local wants_seconds = false
  for _, part in ipairs(template) do
    if part:find("{seconds}", 1, true) then
      wants_seconds = true
      break
    end
  end

  local effective = url
  if seconds and seconds > 0 and not wants_seconds then
    effective = url .. (url:find("?", 1, true) and "&" or "?") .. "t=" .. math.floor(seconds) .. "s"
  end

  local argv = {}
  for _, part in ipairs(template) do
    local out = part:gsub("{url}", (effective:gsub("%%", "%%%%")))
    out = out:gsub("{seconds}", tostring(math.floor(seconds or 0)))
    table.insert(argv, out)
  end
  return argv
end

local function launch_video(url, seconds)
  local argv = expand_player(url, seconds, resolve_player())
  if vim.fn.executable(argv[1]) ~= 1 then
    notify("Player not found: " .. argv[1], vim.log.levels.ERROR)
    return
  end
  local ok, err = pcall(vim.system, argv, { detach = true })
  if not ok then
    notify("Failed to launch player: " .. tostring(err), vim.log.levels.ERROR)
    return
  end
  notify(("Playing %s%s"):format(url, seconds and seconds > 0 and (" at " .. format_hms(seconds)) or ""))
end

local function videos(vault, section_uid)
  local args = { "videos" }
  if section_uid then
    table.insert(args, section_uid)
  end
  local response = editor(vault, args)
  return response and response.items or nil
end

local function video_label(item)
  local stamp = item.seconds and item.seconds > 0 and format_hms(item.seconds) or "--:--"
  local where = item.label ~= "" and item.label or item.note_title
  return string.format("%-9s %s  [%s]", stamp, where, item.url)
end

-- `@video URL 06:54  Label` — returns the URL and the parsed second, or nil.
local function parse_video_line(line)
  local url, trailing = line:match("^%s*@video%s+(%S+)%s*(.*)$")
  if not url then
    return nil
  end
  local stamp = trailing:match("^(%S+)")
  local seconds
  if stamp then
    local h, m, s = stamp:match("^(%d+):(%d+):(%d+)$")
    if h then
      seconds = tonumber(h) * 3600 + tonumber(m) * 60 + tonumber(s)
    else
      local mm, ss = stamp:match("^(%d+):(%d+)$")
      if mm then
        seconds = tonumber(mm) * 60 + tonumber(ss)
      else
        seconds = tonumber(stamp)
      end
    end
  end
  return { url = url, seconds = seconds }
end

--- Play the `@video` on this line, else the URL under the cursor, else the
--- first video in the enclosing section.
function M.play()
  local vault = current_vault()
  if not vault then return end

  local inline = parse_video_line(vim.api.nvim_get_current_line())
  if inline then
    launch_video(inline.url, inline.seconds)
    return
  end

  local under_cursor = vim.fn.expand("<cWORD>"):match("https?://[^%s)>%]]+")
  if under_cursor and (under_cursor:find("youtube%.com") or under_cursor:find("youtu%.be")) then
    launch_video(under_cursor, nil)
    return
  end

  local all = sections(vault)
  if not all then return end
  local section = current_section(vault, all)
  if not section then return end

  local items = videos(vault, section.uid)
  if not items or #items == 0 then
    notify("No @video in this section")
    return
  end
  launch_video(items[1].url, items[1].seconds)
end

--- Browse the whole vault's videos and play one.
function M.videos()
  local vault = current_vault()
  if not vault then return end
  local items = videos(vault)
  if not items then return end
  pick(items, "Videos", video_label, function(item)
    launch_video(item.url, item.seconds)
  end)
end

--- Browse the yClippy library and play a video or clip from it.
function M.library(query)
  if vim.fn.executable(config.clipper) ~= 1 then
    notify("Executable not found: " .. config.clipper, vim.log.levels.ERROR)
    return
  end
  local argv = { config.clipper, "list", "--json" }
  if query and query ~= "" then
    vim.list_extend(argv, { "--query", query })
  end
  local result = vim.system(argv, { text = true }):wait()
  if result.code ~= 0 then
    notify(vim.trim(result.stderr ~= "" and result.stderr or result.stdout), vim.log.levels.ERROR)
    return
  end
  local ok, value = pcall(vim.json.decode, result.stdout)
  if not ok or value.protocol_version ~= 1 then
    notify("Unsupported or invalid yclippy list response", vim.log.levels.ERROR)
    return
  end
  pick(value.items or {}, "Library", function(item)
    return string.format("%-6s %-9s %s", item.kind, format_hms(item.start_seconds), item.title)
  end, function(item)
    launch_video(item.url, item.start_seconds)
  end)
end

--- Pick from the yClippy library and insert an `@video` line at the cursor.
--- This is what closes the loop: clip in yClippy, drop the line into a note,
--- and the moment becomes indexed, graphed, and replayable from three places.
function M.insert(query)
  if vim.fn.executable(config.clipper) ~= 1 then
    notify("Executable not found: " .. config.clipper, vim.log.levels.ERROR)
    return
  end
  local argv = { config.clipper, "list", "--json" }
  if query and query ~= "" then
    vim.list_extend(argv, { "--query", query })
  end
  local result = vim.system(argv, { text = true }):wait()
  if result.code ~= 0 then
    notify(vim.trim(result.stderr ~= "" and result.stderr or result.stdout), vim.log.levels.ERROR)
    return
  end
  local ok, value = pcall(vim.json.decode, result.stdout)
  if not ok or value.protocol_version ~= 1 then
    notify("Unsupported or invalid yclippy list response", vim.log.levels.ERROR)
    return
  end
  pick(value.items or {}, "Library", function(item)
    return string.format("%-6s %-9s %s", item.kind, format_hms(item.start_seconds), item.title)
  end, function(item)
    -- The URL is stored clean; yalive lifts a `t=` back out at index time, so
    -- the timestamp is written as its own field rather than glued into the URL.
    local url = item.url:gsub("[?&]t=[0-9hms]+", "")
    local stamp = item.start_seconds > 0 and (" " .. format_hms(item.start_seconds)) or ""
    insert_line(("@video %s%s  %s"):format(url, stamp, item.title))
  end)
end

local function choose_relation(capabilities, callback)
  pick(capabilities.relation_types, "Relation type", function(item)
    return item.relation_type
  end, callback)
end

local function use_relation(capabilities, relation_type, callback)
  if not relation_type then
    choose_relation(capabilities, callback)
    return
  end
  for _, kind in ipairs(capabilities.relation_types) do
    if kind.relation_type == relation_type then
      callback(kind)
      return
    end
  end
  notify("Unsupported relation type: " .. relation_type, vim.log.levels.ERROR)
end

local function relation_text(kind, target_uid)
  return kind.prefix .. "[[" .. target_uid .. "]]"
end

function M.card()
  local vault = current_vault()
  if not vault then return end
  local capabilities = editor(vault, { "capabilities" })
  if not capabilities then return end
  pick(capabilities.card_types, "Card type", function(item)
    return item.label .. "  [" .. item.card_type .. "]"
  end, function(card)
    vim.ui.input({ prompt = "Card ID: ", default = card.card_type .. "-card" }, function(id)
      if not id or vim.trim(id) == "" then return end
      id = vim.trim(id):lower():gsub("[^a-z0-9_-]+", "-"):gsub("^-", ""):gsub("-$", "")
      local text = card.template:gsub("%$%{id%}", id)
      vim.api.nvim_put(vim.split(text, "\n", { plain = true }), "l", true, true)
    end)
  end)
end

function M.search(query)
  local vault = current_vault()
  if not vault then return end
  local items = sections(vault, query or "")
  if not items then return end
  pick(items, "Sections", section_label, function(item)
    open_section(vault, item)
  end)
end

function M.link(direction, relation_type)
  local vault = current_vault()
  if not vault then return end
  local all_sections = sections(vault, "")
  if not all_sections then return end
  local origin = current_section(vault, all_sections)
  if not origin then return end
  local capabilities = editor(vault, { "capabilities" })
  if not capabilities then return end

  pick(all_sections, direction == "incoming" and "Source section" or "Target section", section_label, function(selected)
    use_relation(capabilities, relation_type, function(kind)
      if direction == "incoming" then
        open_section(vault, selected)
        insert_line(relation_text(kind, origin.uid))
      else
        insert_line(relation_text(kind, selected.uid))
      end
    end)
  end)
end

function M.relations()
  local vault = current_vault()
  if not vault then return end
  local all_sections = sections(vault, "")
  if not all_sections then return end
  local origin = current_section(vault, all_sections)
  if not origin then return end
  local response = editor(vault, { "relations", origin.uid })
  if not response then return end
  local by_uid = {}
  for _, section in ipairs(all_sections) do by_uid[section.uid] = section end
  pick(response.items, "Relations", function(item)
    local arrow = item.incoming and "<-" or "->"
    return string.format("%s %s %s", arrow, item.relation_type, item.target_heading or item.target_uid)
  end, function(item)
    local section = by_uid[item.target_uid]
    if section then
      open_section(vault, section)
    else
      notify("Relation target is unresolved: " .. item.target_uid, vim.log.levels.WARN)
    end
  end)
end

function M.index(silent)
  local vault = vault_for(vim.api.nvim_buf_get_name(0))
  if not vault then return end
  local output = command(vault, { "index" })
  if output and not silent then notify(vim.trim(output)) end
end

function M.diagnostics(bufnr)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  local path = vim.api.nvim_buf_get_name(bufnr)
  local vault = vault_for(path)
  if not vault then return end
  local response = editor(vault, { "diagnostics" })
  if not response then return end
  local relative = relative_path(vault, path)
  local diagnostics = {}
  for _, item in ipairs(response.items) do
    if vim.fs.normalize(item.path) == relative then
      table.insert(diagnostics, {
        lnum = math.max(item.line - 1, 0),
        col = 0,
        message = item.message,
        severity = vim.diagnostic.severity.WARN,
        source = "yalive",
      })
    end
  end
  vim.diagnostic.set(namespace, bufnr, diagnostics)
end

local function create_commands()
  if commands_created then return end
  commands_created = true
  vim.api.nvim_create_user_command("YaliveCard", M.card, {})
  vim.api.nvim_create_user_command("YaliveSearch", function(opts) M.search(opts.args) end, { nargs = "*" })
  vim.api.nvim_create_user_command("YaliveLink", function() M.link("outgoing") end, {})
  vim.api.nvim_create_user_command("YaliveBacklink", function() M.link("incoming") end, {})
  vim.api.nvim_create_user_command("YaliveOutgoingLink", function() M.link("outgoing", "outgoing") end, {})
  vim.api.nvim_create_user_command("YaliveIngoingLink", function() M.link("outgoing", "ingoing") end, {})
  vim.api.nvim_create_user_command("YaliveRelations", M.relations, {})
  vim.api.nvim_create_user_command("YaliveIndex", function() M.index(false) end, {})
  vim.api.nvim_create_user_command("YaliveDiagnostics", function() M.diagnostics(0) end, {})
  vim.api.nvim_create_user_command("YaliveVideos", M.videos, {})
  vim.api.nvim_create_user_command("YalivePlay", M.play, {})
  vim.api.nvim_create_user_command("YaliveLibrary", function(opts) M.library(opts.args) end, { nargs = "*" })
  vim.api.nvim_create_user_command("YaliveInsertClip", function(opts) M.insert(opts.args) end, { nargs = "*" })

  -- The video commands shipped under a `YClippy` prefix, which put two
  -- different names on one plugin's commands. They answer to `Yalive*` now;
  -- the old names stay as aliases so existing configs keep working.
  for old, new in pairs({
    YClippyPlay = "YalivePlay",
    YClippyLibrary = "YaliveLibrary",
    YClippyInsert = "YaliveInsertClip",
  }) do
    vim.api.nvim_create_user_command(old, function(opts)
      vim.cmd(new .. " " .. opts.args)
    end, { nargs = "*" })
  end
end

--- Pure helpers, exposed for `nvim/tests/`. Not part of the public API.
M._internal = {
  format_hms = format_hms,
  expand_player = expand_player,
  resolve_player = resolve_player,
  parse_video_line = parse_video_line,
  set_player = function(template) config.player = template end,
}

function M.setup(options)
  config = vim.tbl_deep_extend("force", config, options or {})
  create_commands()
  local group = vim.api.nvim_create_augroup("yalive", { clear = true })
  if config.auto_index or config.diagnostics then
    vim.api.nvim_create_autocmd("BufWritePost", {
      group = group,
      pattern = "*.md",
      callback = function(event)
        if not vault_for(event.file) then return end
        -- `editor diagnostics` reindexes the vault before it answers, so
        -- running diagnostics already satisfies `auto_index`. Running both
        -- would index the vault twice on every save.
        if config.diagnostics then
          M.diagnostics(event.buf)
        elseif config.auto_index then
          M.index(true)
        end
      end,
    })
  end
end

return M
