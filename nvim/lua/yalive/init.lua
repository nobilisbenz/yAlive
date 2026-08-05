local M = {}

local config = {
  executable = "yalive",
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
end

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
