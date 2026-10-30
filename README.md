# RustyBar

A status bar for macOS, written in Rust: one process that reads everything in-process and is
driven by events, with Core Animation for the animations and popups that you can build out of
real components (grids, scrolling lists, sliders, toggles, progress bars) in Lua.

At rest, with the built-in widgets, it uses about **15 MB** of memory, **~0% CPU**, starts **no
processes**, and draws its first frame in **about 40 ms**.

## Widgets

| Widget | What it shows | Popup |
| --- | --- | --- |
| `apple` | The Apple logo | The Apple menu: About This Mac, Force Quit, Sleep, Restart, Shut Down, Lock Screen, Log Out |
| `aerospace` | [AeroSpace](https://github.com/nikitabobko/AeroSpace) workspaces with the icons of their windows in on-screen order, the focused one under a sliding highlight, the binding mode | — (clicks switch workspace, focus a window, leave the mode) |
| `front_app` | The app in front | — |
| `system` | CPU, GPU and memory, colored by load and memory pressure | Usage bars, details and the top apps for each, Activity Monitor |
| `battery` | The charge, with an icon for its state | State, Charge Limit (macOS 26.4+) and Charge to Full Now, battery health, Battery settings |
| `clock` | Date and time in the language and format of your Mac | A calendar that scrolls through the months without end; a click on a day opens it in Calendar |

Widgets that read nothing new until something changes don't poll: AeroSpace events come from its
socket, the battery from IOKit and PowerUIAgent notifications, the front app from NSWorkspace,
the clock from a timer aligned to the minute. CPU, GPU and memory are read every 2 seconds
in-process, and the bar is redrawn only when a value changes.

Until a widget has its first data, the bar shows its last known state, dimmed, or a shimmering
placeholder the very first time. A widget that fails shows an error pill; the rest of the bar
keeps working.

## Install

Requires macOS 13 or later and Rust (`brew install rustup && rustup-init`).

```bash
git clone https://github.com/Francesco-Rapetti/rustybar && cd rustybar
scripts/bundle.sh
cp -R target/RustyBar.app /Applications/
/Applications/RustyBar.app/Contents/MacOS/rustybar service install
```

`service install` adds a LaunchAgent that starts RustyBar at login and again if it ever crashes;
its log is `~/Library/Logs/rustybar.log`. `service uninstall` removes it.

macOS remembers permissions by code signature. With the default ad hoc signature it asks again
after every build; `scripts/signing-cert.sh` creates a local signing certificate once, and
`scripts/bundle.sh` uses it from then on (or set `RUSTYBAR_SIGN_IDENTITY` to a Developer ID).

To keep the macOS menu bar out of the way, turn on *Automatically hide and show the menu bar* in
System Settings → Control Center.

## Configure

The config is `~/.config/rustybar/rustybar.toml`, written with every default and a comment for
each key on the first start (see [config/rustybar.toml](config/rustybar.toml)). Saving it, or any
widget in `~/.config/rustybar/widgets/`, reloads the bar, with the changes animated.

```toml
[theme]
preset = "catppuccin-mocha"
primary = "mauve"

[layout]
left = ["apple", "aerospace", "front_app"]
right = ["system", "battery", "clock"]

[widgets.clock_utc]   # a second clock, by name
type = "clock"
timezone = "UTC"
```

On screens with a notch the widgets switch to their compact form (`compact_on_notch`), and
`hide_on_notch` lists the ones to leave out there.

## Lua widgets

Every `~/.config/rustybar/widgets/*.lua` file can define widgets; put their names in `[layout]`.

```lua
local rb = require("rustybar")

rb.widget {
  name = "weather",
  every = 600,                       -- seconds between update() calls
  update = function(self)
    rb.exec("curl -s 'wttr.in/?format=%t'", function(stdout, status)
      if status == 0 then self.temp = stdout end   -- the bar redraws after the callback
    end)
  end,
  bar = function(self, ctx)
    return rb.ui.pill { rb.ui.icon { "cloud.sun" }, rb.ui.text { self.temp or "…" }, on_click = "popup" }
  end,
  popup = function(self, ctx)
    return rb.ui.column { min_width = 220,
      rb.ui.section { "Weather", right = rb.ui.text { self.temp or "", color = "primary" } },
      rb.ui.button { rb.ui.text { "Open wttr.in" }, on_click = "https://wttr.in" },
    }
  end,
}
```

- **Widget fields**: `name`, `init(self)`, `update(self)` with `every`, `bar(self, ctx)`,
  `popup(self, ctx)`, `event(self, name, args)` with `on = { "wake", … }`. `ctx` has `compact`,
  `notch`, `large`, `width` and `popup_open`.
- **Components** (`rb.ui.*`): `row`, `column`, `grid` (`columns = n`), `pill`, `button`, `box`,
  `text`, `icon` (an SF Symbol), `app` (an app icon by bundle id), `image`, `slider`, `progress`,
  `toggle`, `scroll` (`max_height`), `skeleton`, `spacer`, `divider`, `section`, `detail`,
  `month`.
- **Options**: `gap`, `pad`, `width`, `height`, `min_width`, `max_width`, `grow`, `radius`,
  `background`, `border`, `opacity`, `align`, `justify`, `key`, `hover`, `selected` (a sliding
  highlight behind the child with that key), colors by palette name (`"primary"`, `"green@0.5"`)
  or hex.
- **Actions**: `on_click` (and `on_change` for sliders, `on_edge` for scrolls) takes a function, a
  URL to open, `"popup"`, or a shell command.
- **Functions**: `rb.exec(cmd, function(stdout, status, stderr) … end)` (in the background), `rb.shell(cmd)`, `rb.open(url)`,
  `rb.redraw()`, `rb.close_popup()`, `rb.trigger(name, args)`, `rb.color(name)`, `rb.theme`.

Lua runs inside RustyBar, sandboxed: no `os.execute`, `io.popen` or `os.exit`, and a call that
takes more than 250 ms is stopped. Anything slow goes through `rb.exec`.

`rustybar check` loads the config and the Lua widgets and reports the first error.

## Command line

```
rustybar msg reload                  reload the config
rustybar msg query                   the widgets, their state and where they are
rustybar msg trigger <event> k=v…    send an event to every widget (Lua: event(self, name, args))
rustybar msg popup <widget>          open or close a popup
rustybar msg quit
```

For example, AeroSpace bindings that move windows can run
`rustybar msg trigger aerospace_windows` so the workspaces update at once.

## License

MIT
