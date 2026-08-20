-- Run with:  nvim --headless -c "luafile nvim/tests/video_spec.lua" -c "qa"
--
-- Covers the pure half of the video integration: reading an `@video` line and
-- expanding a player template. The impure half (spawning) is one `vim.system`
-- call and is left to manual testing.

local root = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h:h:h")
local M = dofile(root .. "/nvim/lua/yalive/init.lua")
local internal = M._internal

local failures = 0

local function check(name, got, want)
  local ok = vim.deep_equal(got, want)
  if not ok then
    failures = failures + 1
    print(("FAIL %s\n  got  %s\n  want %s"):format(name, vim.inspect(got), vim.inspect(want)))
  else
    print("ok   " .. name)
  end
end

-- ── format_hms ────────────────────────────────────────────────────────────
check("414 formats as 6:54", internal.format_hms(414), "6:54")
check("3723 formats as 1:02:03", internal.format_hms(3723), "1:02:03")
check("0 formats as 0:00", internal.format_hms(0), "0:00")

-- ── parse_video_line ──────────────────────────────────────────────────────
check(
  "a trailing label does not swallow the timestamp",
  internal.parse_video_line("@video https://youtu.be/ABC 06:54  Chapter on borrowing"),
  { url = "https://youtu.be/ABC", seconds = 414 }
)
check(
  "hours parse",
  internal.parse_video_line("@video https://youtu.be/ABC 1:02:03"),
  { url = "https://youtu.be/ABC", seconds = 3723 }
)
check(
  "bare seconds parse",
  internal.parse_video_line("@video https://youtu.be/ABC 414"),
  { url = "https://youtu.be/ABC", seconds = 414 }
)
check(
  "no timestamp is fine",
  internal.parse_video_line("@video https://youtu.be/ABC"),
  { url = "https://youtu.be/ABC", seconds = nil }
)
check(
  "a line that is not an @video is not one",
  internal.parse_video_line("see https://youtu.be/ABC for more"),
  nil
)
check(
  "indentation is allowed",
  internal.parse_video_line("   @video https://youtu.be/ABC 1:00"),
  { url = "https://youtu.be/ABC", seconds = 60 }
)

-- ── expand_player ─────────────────────────────────────────────────────────
internal.set_player({ "yclippy", "play", "{url}", "--at", "{seconds}" })
check(
  "seconds placeholder is filled and the URL left alone",
  internal.expand_player("https://youtu.be/ABC", 414),
  { "yclippy", "play", "https://youtu.be/ABC", "--at", "414" }
)

internal.set_player({ "xdg-open", "{url}" })
check(
  "a template without {seconds} rebuilds the timestamp into the URL",
  internal.expand_player("https://youtu.be/ABC", 414),
  { "xdg-open", "https://youtu.be/ABC?t=414s" }
)
check(
  "rebuilding respects an existing query string",
  internal.expand_player("https://www.youtube.com/watch?v=ABC", 90),
  { "xdg-open", "https://www.youtube.com/watch?v=ABC&t=90s" }
)
check(
  "no timestamp means no rebuild",
  internal.expand_player("https://youtu.be/ABC", nil),
  { "xdg-open", "https://youtu.be/ABC" }
)

internal.set_player({ "mpv", "--start={seconds}", "{url}" })
check(
  "substitution does not re-split on whitespace",
  internal.expand_player("https://example.com/a b", 5),
  { "mpv", "--start=5", "https://example.com/a b" }
)

-- A URL with a `%` must survive gsub's replacement escaping.
check(
  "percent signs in a URL survive substitution",
  internal.expand_player("https://example.com/a%20b", 5),
  { "mpv", "--start=5", "https://example.com/a%20b" }
)

-- ── resolve_player ────────────────────────────────────────────────────────
-- The chain must always end somewhere runnable, and must never return an empty
-- template, whatever happens to be installed on the machine running this.
do
  internal.set_player(nil)
  local resolved = internal.resolve_player()
  check("resolution yields a non-empty template", type(resolved) == "table" and #resolved > 0, true)
  local has_url = false
  for _, part in ipairs(resolved) do
    if part:find("{url}", 1, true) then has_url = true end
  end
  check("resolution yields a template carrying {url}", has_url, true)

  -- A configured player that is not installed must not become the answer.
  internal.set_player({ "definitely-not-installed-xyz", "{url}" })
  check(
    "a missing configured player falls back",
    internal.resolve_player()[1] ~= "definitely-not-installed-xyz",
    true
  )
  internal.set_player(nil)
end

print(failures == 0 and "\nall passed" or ("\n" .. failures .. " failed"))
if failures > 0 then
  vim.cmd("cq")
end
