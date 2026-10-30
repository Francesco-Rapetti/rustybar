# RustyBar

A status bar for macOS, written in Rust: one process that reads everything in-process and is
driven by events, with Core Animation for the animations and popups that you can build out of
real components (grids, scrolling lists, sliders, toggles, progress bars) in Lua.

At rest, with every built-in widget in the bar, it uses about **18 MB** of memory and **~0.1%
CPU** in a single process, and draws its first frame in **about 50 ms**. For comparison, the
SketchyBar setup it replaces took about 176 MB in 15 processes and ~1.3% CPU, and started some 150
processes a minute.

## Widgets

| Widget | What it shows | Popup |
| --- | --- | --- |
| `apple` | The Apple logo | The Apple menu: About This Mac, Force Quit, Sleep, Restart, Shut Down, Lock Screen, Log Out |
| `aerospace` | [AeroSpace](https://github.com/nikitabobko/AeroSpace) workspaces with the icons of their windows in on-screen order, the focused one under a sliding highlight, the binding mode | — (clicks switch workspace, focus a window, leave the mode) |
| `front_app` | The app in front | — |
| `system` | CPU, GPU and memory, colored by load and memory pressure | Usage bars, details and the top apps for each, Activity Monitor |
| `battery` | The charge, with an icon for its state | State, Charge Limit (macOS 26.4+) and Charge to Full Now, battery health, Battery settings |
| `clock` | Date and time in the language and format of your Mac | A calendar that scrolls through the months without end; a click on a day opens it in Calendar |
| `audio` | The output and the input in use, with the battery of Bluetooth devices; the Mac's own speakers and microphone, or both halves of a headset, show as one | The volume of each, the devices to switch to, the paired Bluetooth headphones and speakers to connect or disconnect |
| `network` | The VPN, then the Wi-Fi network and its band, or Ethernet; the icon turns yellow or red when there's no connection | Addresses (local and public), router and VPN, wired services, Wi-Fi on and off, the nearby networks to join (the password is asked once and kept in the keychain) |
| `events` | Today's next event from the Mac's calendars: its time, colored as it gets close, the title and a countdown | Today's events; a click joins the Google Meet call or shows the event in Calendar |
| `notifications` | Two counts: the Dock badges of the apps you choose and new RustyBar crash reports (green), Homebrew updates (red) | The badged apps and crash reports to open, the packages to update one by one or all at once |

Widgets don't poll what macOS can tell them: AeroSpace events come from its socket, the battery
from IOKit and PowerUIAgent notifications, the front app from NSWorkspace, the sound from CoreAudio
listeners, the network from the System Configuration store, the calendars from EventKit, the clock
from a timer aligned to the minute. What has no event is read in-process on a timer: CPU, GPU and
memory every 2 seconds, the Dock badges every 2 seconds (through Accessibility). The bar is redrawn
only when a value changes.

A few things still run a process: `brew outdated` every hour and on wake (only with `brewfile` or
`brew = true`), `system_profiler` for AirPods batteries and paired devices when the audio popup
opens and every 2 minutes while a Bluetooth device is in use, and `curl` for the public IP when the
network popup opens.

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

RustyBar asks for each permission the first time a widget needs it:

| Permission | Widget | For |
| --- | --- | --- |
| Calendars | `events` | Today's events (at start) |
| Location | `network` | The nearby Wi-Fi networks and joining them (when its popup opens); the name of the current network doesn't need it |
| Bluetooth | `audio` | Connecting and disconnecting headphones and speakers (on the first click) |
| Accessibility | `notifications` | The Dock badges (at start, only with `apps`); without it the rest of the widget works |
| Automation | `apple`, `clock` | Restart, Shut Down and Log Out; showing a day in Calendar |

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

## Made-up data

With `RUSTYBAR_MOCK` set to a folder, the widgets that read personal data (`audio`, `network`,
`events`, `notifications`) read `<folder>/<widget>.json` instead and their actions do nothing, for
screenshots and demos. [examples/mock](examples/mock) has one file for each:

```bash
RUSTYBAR_MOCK=examples/mock target/release/rustybar
```

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
