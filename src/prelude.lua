-- rb.ui: builders for the tables RustyBar turns into nodes. Each one tags its table with the kind
-- of node; the rest of the table is the node's options and, in the array part, its children
-- (or, for text, icon, app and image, the text, symbol, bundle id or path).
local rb = require("rustybar")
rb.ui = {}

local function kind(t)
  return function(spec)
    if type(spec) ~= "table" then spec = { spec } end
    spec._t = t
    return spec
  end
end

for _, t in ipairs({ "row", "column", "grid", "pill", "button", "text", "icon", "app", "image",
                     "slider", "progress", "toggle", "scroll", "skeleton", "box" }) do
  rb.ui[t] = kind(t)
end

function rb.ui.spacer() return { _t = "spacer" } end
function rb.ui.divider() return { _t = "divider" } end

-- A title with an optional value on the right, as in the built-in popups
function rb.ui.section(spec)
  if type(spec) ~= "table" then spec = { spec } end
  return rb.ui.row { pad = { 8, 8, 4, 8 },
    rb.ui.text { spec[1] }, rb.ui.spacer(), spec.right }
end

-- Small grey text under a row
function rb.ui.detail(text)
  return rb.ui.text { text, size = rb.theme.small_size, weight = "medium", color = "subtext", pad = { 0, 8, 2, 8 } }
end

-- One month as a 7-column grid, starting on `first` (1 = Sunday, 2 = Monday…)
function rb.ui.month(spec)
  local year, month = spec.year, spec.month
  local first = spec.first or 2
  local days = os.date("*t", os.time { year = year, month = month + 1, day = 0 }).day
  local wday = os.date("*t", os.time { year = year, month = month, day = 1 }).wday
  local cells = { _t = "grid", columns = 7, gap = 2, key = spec.key }
  for _ = 1, (wday - first) % 7 do cells[#cells + 1] = rb.ui.box { height = 24 } end
  local today = os.date("*t")
  for d = 1, days do
    local is_today = today.year == year and today.month == month and today.day == d
    cells[#cells + 1] = rb.ui.box { height = 24, radius = 12, justify = "center",
      background = is_today and "primary" or nil,
      rb.ui.text { tostring(d), color = is_today and "on_primary" or "text" } }
  end
  return cells
end

return rb
