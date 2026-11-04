//! CPU, GPU and memory usage, read every 2 seconds in-process and drawn only when a value changes.
//! CPU and GPU turn yellow from 75% and red from 90%; the memory has the color of its pressure, as
//! in Activity Monitor: macOS keeps the memory nearly full with caches and compressed pages, so the
//! percentage alone says little. The GPU hides when macOS doesn't report it, CPU and GPU in the
//! compact form.
//!
//! The popup is a dashboard of tiles:
//!
//! - CPU and GPU side by side (the CPU two tiles wide on a Mac whose GPU macOS doesn't report):
//!   the usage, colored as in the bar, the cores, and the last minute as bars, one for each
//!   reading in the color of its value. The readings are kept from the start, so the graphs are
//!   full as soon as the popup opens.
//! - Memory, two tiles wide: used of the total in the color of the pressure, the pressure, the swap
//!   when there is some, and a bar split into app memory, wired and compressed, with their sizes.
//! - Storage: a tile for each disk the Finder shows that can be written, the startup disk first,
//!   with used of the total, what's available as the Finder counts it (free, and what macOS can
//!   purge), the purgeable part, and a bar where it's lighter. Yellow from 90% used, red from 95%.
//!   A click opens the disk in the Finder. Disk images, network shares and the hidden volumes of
//!   the system are left out.
//! - The apps that use the most CPU, GPU or memory, one list with a switch above it, and a line
//!   for the one shown: system, user and idle time for the CPU, the model of the GPU.
//! - Activity Monitor and the Storage settings.
//!
//! The lists are read only while the popup is open: the CPU of each process from `ps`, which is
//! setuid and sees the system processes too, the GPU time from the IORegistry and the memory from
//! libproc (only the user's processes: the others are closed to it). The disks come from the mount
//! table, which the kernel gives without waiting on any of them (a network share that doesn't
//! answer can't hang the read), and their sizes from Foundation, which asks macOS what it can
//! purge (a few ms): at the start, so the first popup has them, then when it opens and every
//! 10 seconds while it stays open.
//! Options: `warning` (75) and `critical` (90) for CPU and GPU, `storage_warning` (90) and
//! `storage_critical` (95) for the disks, in percent used.
//! With made-up data (see `mock`) the numbers, the GPU, the graphs, the lists and the disks come
//! from `system.json`.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use objc2_foundation::{NSArray, NSBundle, NSFileManager, NSNumber, NSProcessInfo, NSString, NSURL};
use serde::Deserialize;

use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::iokit;
use crate::runtime;
use crate::ui::{Align, Justify, Kind, Node};

const INTERVAL: f64 = 2.0;
const TOP: usize = 5;
/// The tiles of the popup, two side by side as in `network` and `docker`.
const TILE: f64 = 160.0;
const GAP: f64 = 8.0;
const WIDE: f64 = TILE * 2.0 + GAP;
/// The padding of a tile, left and right.
const PAD: f64 = 10.0;
/// Readings in the graphs of CPU and GPU, a bar each: the last minute.
const BARS: usize = 30;
const GRAPH: f64 = 24.0;
/// The bars of the memory and the disks.
const BAR: f64 = 8.0;
const ROW: f64 = 22.0;
/// While the popup is open the disks are read again every this many readings: 10 seconds.
const STORAGE_EVERY: u32 = 5;
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Clone, Copy, PartialEq, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Pressure {
    #[default]
    Normal,
    Warning,
    Critical,
}

/// What the bar shows; compared with the last one so an unchanged reading draws nothing.
#[derive(Clone, PartialEq, Debug, Default)]
struct Values {
    cpu: Option<u32>,
    gpu: Option<u32>,
    ram: Option<u32>,
    pressure: Pressure,
}

#[derive(Clone, Debug, Default)]
struct App {
    bundle: String,
    name: String,
}

/// The lists of the popup: the apps and their share of the CPU and GPU, or their bytes.
#[derive(Clone, Debug, Default)]
struct Details {
    cpu_top: Vec<(App, f64)>,
    gpu_top: Vec<(App, f64)>,
    memory_top: Vec<(App, f64)>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
struct Memory {
    used: f64,
    app: f64,
    wired: f64,
    compressed: f64,
}

/// A disk, in bytes.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Volume {
    name: String,
    path: String,
    total: f64,
    /// As the Finder counts it: what's free and what macOS can purge.
    available: f64,
    purgeable: f64,
    internal: bool,
}

impl Volume {
    fn used(&self) -> f64 {
        (self.total - self.available).max(0.0)
    }
}

/// The list of apps the popup shows.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum List {
    #[default]
    Cpu,
    Gpu,
    Memory,
}

/// Made-up data (see `mock`): percentages, fractions for user, system and idle, memory in GB,
/// the disks in GB as the Finder counts them (10⁹ bytes), the last readings of CPU and GPU, and
/// the lists as `[name, bundle id, value]` (a percentage, or GB for the memory).
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    cpu: u32,
    gpu: Option<u32>,
    ram: u32,
    pressure: Pressure,
    total: f64,
    cores: usize,
    gpu_model: String,
    gpu_cores: Option<i64>,
    user: f64,
    system: f64,
    idle: f64,
    memory: Memory,
    swap: f64,
    cpu_history: Vec<u32>,
    gpu_history: Vec<u32>,
    storage: Vec<Volume>,
    cpu_top: Vec<(String, String, f64)>,
    gpu_top: Vec<(String, String, f64)>,
    memory_top: Vec<(String, String, f64)>,
}

pub struct System {
    warning: u32,
    critical: u32,
    storage_warning: f64,
    storage_critical: f64,
    values: Values,
    ticks: Option<[u32; 4]>,
    loaded: bool,
    total: f64,
    cores: usize,
    gpu_model: String,
    gpu_cores: Option<i64>,
    /// The last reading of the CPU time (user, system, idle), the memory and the swap.
    time: Option<(f64, f64, f64)>,
    memory: Option<Memory>,
    swap: f64,
    /// The last minute of readings, numbered so the bars slide: CPU and GPU.
    history: VecDeque<(u64, u32, Option<u32>)>,
    readings: u64,
    volumes: Vec<Volume>,
    reading_volumes: bool,
    /// Readings since the popup opened, to read the disks again every `STORAGE_EVERY`.
    open_ticks: u32,
    list: List,
    /// While the popup is open: the lists, and the GPU time of each process at the last reading.
    details: Option<Details>,
    gpu_times: Option<(HashMap<i32, u64>, Instant)>,
    reading: bool,
    /// The lists of the made-up data, always shown.
    mock: Option<Details>,
}

impl System {
    pub fn new(options: &Options) -> System {
        let mut s = System {
            warning: options.f64("warning").unwrap_or(75.0) as u32,
            critical: options.f64("critical").unwrap_or(90.0) as u32,
            storage_warning: options.f64("storage_warning").unwrap_or(90.0),
            storage_critical: options.f64("storage_critical").unwrap_or(95.0),
            values: Values::default(),
            ticks: None,
            loaded: false,
            total: NSProcessInfo::processInfo().physicalMemory() as f64,
            cores: NSProcessInfo::processInfo().activeProcessorCount(),
            gpu_model: String::new(),
            gpu_cores: None,
            time: None,
            memory: None,
            swap: 0.0,
            history: VecDeque::new(),
            readings: 0,
            volumes: Vec::new(),
            reading_volumes: false,
            open_ticks: 0,
            list: List::Cpu,
            details: None,
            gpu_times: None,
            reading: false,
            mock: None,
        };
        if let Some(m) = crate::mock::load::<Mock>("system") {
            let list = |l: Vec<(String, String, f64)>, unit: f64| l.into_iter().map(|(name, bundle, v)| (App { bundle, name }, v * unit)).collect();
            s.values = Values { cpu: Some(m.cpu), gpu: m.gpu, ram: Some(m.ram), pressure: m.pressure };
            s.total = m.total * GB;
            s.cores = m.cores;
            s.gpu_model = m.gpu_model;
            s.gpu_cores = m.gpu_cores;
            s.time = Some((m.user, m.system, m.idle));
            s.memory = Some(Memory { used: m.memory.used * GB, app: m.memory.app * GB, wired: m.memory.wired * GB, compressed: m.memory.compressed * GB });
            s.swap = m.swap * GB;
            s.history = m.cpu_history.iter().enumerate().map(|(i, &cpu)| (i as u64, cpu, m.gpu.and(m.gpu_history.get(i).copied()))).collect();
            s.readings = s.history.len() as u64;
            s.volumes = m.storage.into_iter().map(|v| Volume { total: v.total * 1e9, available: v.available * 1e9, purgeable: v.purgeable * 1e9, ..v }).collect();
            s.mock = Some(Details { cpu_top: list(m.cpu_top, 1.0), gpu_top: list(m.gpu_top, 1.0), memory_top: list(m.memory_top, GB) });
            s.loaded = true;
        }
        s
    }

    fn tick(&mut self, cx: &mut Cx) {
        let new = cpu_ticks();
        let time = match (self.ticks, new) {
            (Some(old), Some(new)) => Some(cpu_time(old, new)),
            _ => None,
        };
        self.ticks = new;
        let memory = memory();
        let values = Values {
            cpu: time.map(|(_, _, idle)| ((1.0 - idle) * 100.0).round() as u32),
            gpu: gpu_utilization(),
            ram: memory.map(|m| (m.used / self.total * 100.0).round() as u32),
            pressure: pressure(),
        };
        if let Some(cpu) = values.cpu {
            self.readings += 1;
            self.history.push_back((self.readings, cpu, values.gpu));
            while self.history.len() > BARS {
                self.history.pop_front();
            }
        }
        if time.is_some() {
            self.time = time;
        }
        self.memory = memory.or(self.memory);
        if values != self.values {
            self.values = values;
            cx.redraw();
        }
        // The first reading has no CPU yet: the bar waits for the second, a moment later
        if time.is_some() {
            self.loaded = true;
        }
        if cx.popup_open {
            self.swap = swap_used();
            self.open_ticks += 1;
            if self.open_ticks.is_multiple_of(STORAGE_EVERY) {
                self.read_volumes(cx);
            }
            if !self.reading {
                self.read_details(cx);
            }
        }
    }

    /// The lists of the popup, on a worker thread: `ps` and libproc take a few milliseconds.
    fn read_details(&mut self, cx: &mut Cx) {
        self.reading = true;
        let before = self.gpu_times.clone();
        cx.background::<System, _>(
            move || {
                let now = Instant::now();
                let after = gpu_times();
                let gpu = match &before {
                    Some((before, at)) => {
                        let secs = now.duration_since(*at).as_secs_f64().max(0.1);
                        after
                            .iter()
                            .filter_map(|(pid, t)| before.get(pid).filter(|b| t >= *b).map(|b| (*pid, (t - b) as f64 / 1e9 / secs * 100.0)))
                            .collect()
                    }
                    None => HashMap::new(),
                };
                let cores = NSProcessInfo::processInfo().activeProcessorCount() as f64;
                let mut owners = HashMap::new();
                let details = Details {
                    // ps counts each core as 100%, the bar all of them
                    cpu_top: top(&process_cpu().into_iter().map(|(p, v)| (p, v / cores)).collect(), 0.1, &mut owners),
                    gpu_top: top(&gpu, 0.1, &mut owners),
                    memory_top: top(&process_memory(), 1024.0 * 1024.0, &mut owners),
                };
                (details, (after, now))
            },
            |w, (details, times), cx| {
                w.reading = false;
                w.gpu_times = Some(times);
                if cx.popup_open {
                    w.details = Some(details);
                    cx.redraw();
                }
            },
        );
    }

    /// The disks, on a worker thread: Foundation takes a few milliseconds to say what can be
    /// purged.
    fn read_volumes(&mut self, cx: &mut Cx) {
        if self.reading_volumes {
            return;
        }
        self.reading_volumes = true;
        cx.background::<System, _>(volumes, |w, volumes, cx| {
            w.reading_volumes = false;
            if volumes != w.volumes {
                w.volumes = volumes;
                if cx.popup_open {
                    cx.redraw();
                }
            }
        });
    }

    fn level(&self, rx: &Rx, v: u32, normal: Rgba) -> Rgba {
        rx.theme.level(v as f64, self.warning as f64, self.critical as f64, normal)
    }

    fn pressure_color(&self, rx: &Rx, normal: Rgba) -> Rgba {
        match self.values.pressure {
            Pressure::Normal => normal,
            Pressure::Warning => rx.theme.yellow,
            Pressure::Critical => rx.theme.red,
        }
    }

    /// CPU or GPU: the usage and the last minute as bars, the newest on the right.
    fn gauge(&self, rx: &Rx, gpu: bool, width: f64) -> Node {
        let th = rx.theme;
        let cores = |n: i64| format!("{n} {}", t("core", "cores"));
        let (key, symbol, note, value) = if gpu {
            ("gpu", "cube", self.gpu_cores.map(cores).unwrap_or_default(), self.values.gpu.unwrap_or(0))
        } else {
            ("cpu", "cpu", cores(self.cores as i64), self.values.cpu.unwrap_or(0))
        };
        let color = self.level(rx, value, th.primary);
        let bar = (width - 2.0 * PAD - (BARS - 1) as f64 * 1.5) / BARS as f64;
        let graph = self
            .history
            .iter()
            .filter_map(|&(n, cpu, g)| Some((n, if gpu { g? } else { cpu })))
            .map(|(n, v)| {
                let height = (2.0 + (GRAPH - 2.0) * v.min(100) as f64 / 100.0).round();
                Node::default().size(bar, height).radius(1.0).background(self.level(rx, v, th.primary)).key(format!("bar-{n}"))
            })
            .collect();
        Node::column(vec![
            heading(rx, symbol, color, &key.to_uppercase(), (note, th.subtext), width),
            Node::text(format!("{value}%"), th.font_with(Weight::Medium, 15.0), self.level(rx, value, th.text)).key("value"),
            Node::row(graph).gap(1.5).justify(Justify::End).align(Align::End).height(GRAPH).key("graph"),
        ])
        .gap(4.0)
        .padding(9.0, PAD, 9.0, PAD)
        .width(width)
        .radius(10.0)
        .background(th.tile(color, value >= self.warning))
        .key(key)
    }

    /// Used of the total, the pressure, and what the memory is used for.
    fn memory_tile(&self, rx: &Rx, ram: u32) -> Node {
        let th = rx.theme;
        let color = self.pressure_color(rx, th.primary);
        let (pressure, note) = match self.values.pressure {
            Pressure::Normal => (t("Pressione normale", "Normal pressure"), th.green),
            Pressure::Warning => (t("Pressione alta", "High pressure"), th.yellow),
            Pressure::Critical => (t("Pressione critica", "Critical pressure"), th.red),
        };
        let used = self.memory.map(|m| m.used).unwrap_or(ram as f64 / 100.0 * self.total);
        let total = format!("{} {} GB", t("di", "of"), (self.total / GB).round());
        let mut value = vec![amount(rx, size(used), total, self.pressure_color(rx, th.text))];
        if self.swap > 0.0 {
            value.push(Node::spacer());
            value.push(Node::text(format!("Swap {}", size(self.swap)), th.small_font(), th.subtext).key("swap"));
        }
        let mut rows = vec![heading(rx, "memorychip", color, t("Memoria", "Memory"), (pressure.to_string(), note), WIDE), Node::row(value).key("used")];
        let compressed = th.color("teal").unwrap_or(th.green);
        match self.memory {
            Some(m) => {
                let parts = [("app", m.app, th.primary), ("wired", m.wired, th.peach), ("compressed", m.compressed, compressed)];
                rows.push(split_bar(rx, &parts, self.total));
                let item = |key: &str, color: Rgba, label: &str, bytes: f64| {
                    Node::row(vec![
                        Node::default().size(7.0, 7.0).radius(3.5).background(color).key("dot"),
                        Node::text(label, th.small_font(), th.subtext).key("label"),
                        Node::text(size(bytes), th.small_font(), th.text).key("value"),
                    ])
                    .gap(4.0)
                    .key(key)
                };
                rows.push(
                    Node::row(vec![
                        item("app", th.primary, "App", m.app),
                        item("wired", th.peach, "Wired", m.wired),
                        item("compressed", compressed, t("Compressa", "Compressed"), m.compressed),
                    ])
                    .justify(Justify::Between)
                    .key("legend"),
                );
            }
            None => rows.push(split_bar(rx, &[("app", used, color)], self.total)),
        }
        Node::column(rows)
            .gap(6.0)
            .padding(9.0, PAD, 10.0, PAD)
            .width(WIDE)
            .radius(10.0)
            .background(th.tile(color, self.values.pressure != Pressure::Normal))
            .key("memory")
    }

    /// A disk: used of the total, what's available and what of it macOS can purge.
    fn volume_tile(&self, rx: &Rx, i: usize, v: &Volume) -> Node {
        let th = rx.theme;
        let percent = if v.total > 0.0 { v.used() / v.total * 100.0 } else { 0.0 };
        let level = |normal: Rgba| th.level(percent, self.storage_warning, self.storage_critical, normal);
        let color = level(th.primary);
        let symbol = if v.internal { "internaldrive" } else { "externaldrive" };
        let available = format!("{} {}", disk_size(v.available), t("disponibili", "available"));
        let mut value = vec![amount(rx, disk_size(v.used()), format!("{} {}", t("di", "of"), disk_size(v.total)), level(th.text))];
        // As the Finder: only when there's something to say
        if v.purgeable >= 0.05e9 {
            value.push(Node::spacer());
            value.push(Node::text(format!("{} {}", disk_size(v.purgeable), t("eliminabili", "purgeable")), th.small_font(), th.subtext).key("purgeable"));
        }
        let parts = [("used", v.used(), color), ("purgeable", v.purgeable, color.faded(0.4))];
        Node::column(vec![heading(rx, symbol, color, &v.name, (available, th.subtext), WIDE), Node::row(value).key("used"), split_bar(rx, &parts, v.total)])
            .gap(6.0)
            .padding(9.0, PAD, 10.0, PAD)
            .width(WIDE)
            .radius(10.0)
            .background(th.tile(color, percent >= self.storage_warning))
            .click(rx.action(&format!("open:{i}")))
            .key(format!("volume-{}", v.path))
    }

    /// The apps that use the most of what the switch on top picks, as tall as `TOP` of them so
    /// the popup doesn't change size with it.
    fn apps(&self, rx: &Rx) -> Node {
        let th = rx.theme;
        let has_gpu = self.values.gpu.is_some();
        let list = if self.list == List::Gpu && !has_gpu { List::Cpu } else { self.list };
        let mut options = vec![(List::Cpu, "cpu", "CPU")];
        if has_gpu {
            options.push((List::Gpu, "gpu", "GPU"));
        }
        options.push((List::Memory, "memory", t("Memoria", "Memory")));
        let inner = WIDE - 2.0 * PAD;
        let n = options.len() as f64;
        let segment = (inner - 4.0 - (n - 1.0) * 2.0) / n;
        let mut selected = None;
        let segments = options
            .iter()
            .map(|&(l, key, label)| {
                let on = l == list;
                if on {
                    selected = Some(key.to_string());
                }
                let mut s = Node::row(vec![Node::text(label, th.small_font(), if on { th.on_primary } else { th.text })])
                    .justify(Justify::Center)
                    .size(segment, 22.0)
                    .radius(6.0)
                    .click(rx.action(&format!("list:{key}")))
                    .key(key);
                if on {
                    s = s.hover(th.on_primary.faded(0.12));
                }
                s
            })
            .collect();
        let switch = Node::row(segments).gap(2.0).padding(2.0, 2.0, 2.0, 2.0).radius(8.0).background(th.surface).selected(selected, th.primary).key("switch");
        let line = match list {
            List::Cpu => self
                .time
                .map(|(user, system, idle)| format!("{} {} · {} {} · {} {}", t("Sistema", "System"), pct(system), t("Utente", "User"), pct(user), t("Inattivo", "Idle"), pct(idle)))
                .unwrap_or_default(),
            List::Gpu => gpu_name(self.gpu_model.clone(), self.gpu_cores),
            List::Memory => t("Solo i tuoi processi", "Your processes only").to_string(),
        };
        let details = self.details.as_ref().or(self.mock.as_ref());
        let apps: &[(App, f64)] = match (details, list) {
            (Some(d), List::Cpu) => &d.cpu_top,
            (Some(d), List::Gpu) => &d.gpu_top,
            (Some(d), List::Memory) => &d.memory_top,
            (None, _) => &[],
        };
        let format = |v: f64| if list == List::Memory { size(v) } else { pct(v / 100.0) };
        let prefix = options.iter().find(|o| o.0 == list).map_or("cpu", |o| o.1);
        let mut rows: Vec<Node> = apps
            .iter()
            .map(|(app, v)| {
                let icon = if app.bundle.is_empty() { Node::default().size(16.0, 16.0) } else { Node::app_icon(&app.bundle, 16.0) };
                let value = format(*v);
                let room = inner - 16.0 - 16.0 - crate::gfx::measure_text(&value, &th.font()).width - 8.0;
                Node::row(vec![
                    icon.key("icon"),
                    Node::text(&app.name, th.font(), th.text).max_width(room).key("name"),
                    Node::spacer(),
                    Node::text(value, th.font_with(Weight::Medium, th.font_size), th.subtext).key("value"),
                ])
                .gap(8.0)
                .height(ROW)
                .key(format!("{prefix}-{}", app.name))
            })
            .collect();
        if details.is_none() {
            rows = (0..3).map(|i| Node::row(vec![Node::new(Kind::Skeleton).size(inner * [0.8, 0.6, 0.7][i], 12.0).radius(4.0)]).height(ROW).key(format!("skeleton-{i}"))).collect();
        }
        Node::column(vec![
            switch,
            Node::text(line, th.small_font(), th.subtext).max_width(inner).padding(2.0, 2.0, 0.0, 2.0).key("line"),
            Node::column(rows).height(TOP as f64 * ROW).key(format!("list-{prefix}")),
        ])
        .gap(6.0)
        .padding(PAD, PAD, 6.0, PAD)
        .width(WIDE)
        .radius(10.0)
        .background(th.tile(th.primary, false))
        .key("apps")
    }
}

impl Widget for System {
    fn start(&mut self, cx: &mut Cx) {
        if self.mock.is_some() {
            return;
        }
        let gpus = iokit::services(c"IOAccelerator");
        self.gpu_model = gpus.iter().find_map(|g| g.string("model")).unwrap_or_default();
        self.gpu_cores = gpus.iter().find_map(|g| g.number("gpu-core-count"));
        self.tick(cx);
        // The CPU needs two readings: the second one soon, so the bar doesn't wait 2 seconds
        cx.after::<System>(0.25, |w, cx| w.tick(cx));
        cx.every::<System>(INTERVAL, |w, cx| w.tick(cx));
        // So the first popup has the disks when it opens
        self.read_volumes(cx);
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let v = &self.values;
        // Each number as wide as "100%", so the bar doesn't move when one gains a digit
        let wide = crate::gfx::measure_text("100%", &th.font()).width.ceil();
        let stat = |key: &str, icon: &str, value: u32, color: Rgba, fixed: bool| {
            let mut label = Node::row(vec![Node::text(format!("{value}%"), th.font(), color)]).key("v");
            if fixed {
                label = label.min_width(wide);
            }
            Node::row(vec![Node::symbol(icon, th.icon_size, Weight::Semibold, color), label]).gap(4.0).key(key)
        };
        let mut items = Vec::new();
        if !rx.compact {
            if let Some(cpu) = v.cpu {
                items.push(stat("cpu", "cpu", cpu, self.level(rx, cpu, th.text), true));
            }
            if let Some(gpu) = v.gpu {
                items.push(stat("gpu", "cube", gpu, self.level(rx, gpu, th.text), true));
            }
        }
        if let Some(ram) = v.ram {
            items.push(stat("ram", "memorychip", ram, self.pressure_color(rx, th.text), false));
        }
        if items.is_empty() {
            return None;
        }
        Some(rx.pill(items).gap(8.0).click(rx.toggle_popup()))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let v = &self.values;
        let mut rows = Vec::new();
        let mut gauges = vec![self.gauge(rx, false, if v.gpu.is_some() { TILE } else { WIDE })];
        if v.gpu.is_some() {
            gauges.push(self.gauge(rx, true, TILE));
        }
        rows.push(Node::row(gauges).gap(GAP).key("gauges"));
        if let Some(ram) = v.ram {
            rows.push(self.memory_tile(rx, ram));
        }
        for (i, volume) in self.volumes.iter().enumerate() {
            rows.push(self.volume_tile(rx, i, volume));
        }
        rows.push(self.apps(rx));
        rows.push(
            Node::column(vec![
                rx.row_button(vec![Node::text(t("Apri Monitoraggio Attività", "Open Activity Monitor"), th.font(), th.primary)], vec![], rx.action("activity")).key("activity"),
                rx.row_button(vec![Node::text(t("Impostazioni Archiviazione…", "Storage Settings…"), th.font(), th.primary)], vec![], rx.action("storage")).key("storage"),
            ])
            .key("buttons"),
        );
        Some(rx.popup(WIDE, rows).gap(GAP).padding(8.0, 8.0, 6.0, 8.0))
    }

    fn ready(&self) -> bool {
        self.loaded
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        if let Some(list) = name.strip_prefix("list:") {
            self.list = match list {
                "gpu" => List::Gpu,
                "memory" => List::Memory,
                _ => List::Cpu,
            };
            cx.redraw();
            return;
        }
        let volume = name.strip_prefix("open:").and_then(|i| self.volumes.get(i.parse::<usize>().ok()?)).map(|v| v.path.clone());
        if !matches!(name, "activity" | "storage") && volume.is_none() {
            return;
        }
        cx.close_popup();
        if self.mock.is_some() {
            return;
        }
        match (name, volume) {
            ("activity", _) => runtime::open_app("com.apple.ActivityMonitor"),
            ("storage", _) => runtime::open("x-apple.systempreferences:com.apple.settings.Storage"),
            (_, Some(path)) => runtime::open(&path),
            _ => {}
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        if self.mock.is_some() {
            return;
        }
        match event {
            // The lists need two readings of the GPU time: the first now, the details soon after
            Event::PopupOpened => {
                self.gpu_times = Some((gpu_times(), Instant::now()));
                self.swap = swap_used();
                self.open_ticks = 0;
                self.read_volumes(cx);
                cx.after::<System>(0.25, |w, cx| w.tick(cx));
            }
            Event::PopupClosed => {
                self.details = None;
                self.gpu_times = None;
            }
            // The graphs start again: the minute before the sleep isn't the last one
            Event::Wake => {
                self.ticks = None;
                self.history.clear();
            }
            _ => {}
        }
    }

    crate::any_widget!();
}

/// The top of a tile: an icon and a label, and a note on the right. The note gets the room the
/// label leaves, but at least half.
fn heading(rx: &Rx, symbol: &str, color: Rgba, label: &str, (note, note_color): (String, Rgba), width: f64) -> Node {
    let th = rx.theme;
    let font = th.small_font();
    let room = width - 2.0 * PAD - 14.0 - 4.0 - 8.0;
    let note_width = crate::gfx::measure_text(&note, &font).width.min(room / 2.0);
    let label_width = crate::gfx::measure_text(label, &font).width.min(room - note_width);
    Node::row(vec![
        Node::row(vec![Node::symbol(symbol, 10.0, Weight::Bold, color)]).width(14.0).justify(Justify::Center).key("icon"),
        Node::text(label, font.clone(), th.subtext).max_width(room - note_width).key("label"),
        Node::spacer(),
        Node::text(note, font, note_color).max_width(room - label_width).key("note"),
    ])
    .gap(4.0)
    .height(14.0)
    .key("top")
}

/// A size in large type and what it's of in small type, on the same baseline: `20,5 GB di 32 GB`.
fn amount(rx: &Rx, value: String, of: String, color: Rgba) -> Node {
    let th = rx.theme;
    Node::row(vec![
        Node::text(value, th.font_with(Weight::Medium, 15.0), color).key("value"),
        Node::text(of, th.small_font(), th.subtext).padding(0.0, 0.0, 1.0, 0.0).key("of"),
    ])
    .gap(4.0)
    .align(Align::End)
    .key("amount")
}

/// A bar as wide as the tile, split into parts of `total` with a hairline between them, the rest
/// as the track.
fn split_bar(rx: &Rx, parts: &[(&str, f64, Rgba)], total: f64) -> Node {
    let width = WIDE - 2.0 * PAD;
    let shown: Vec<&(&str, f64, Rgba)> = parts.iter().filter(|p| total > 0.0 && p.1 / total * width >= 1.0).collect();
    let room = width - shown.len() as f64 * 2.0;
    let mut children: Vec<Node> = shown.iter().map(|(key, v, color)| Node::default().size((room * v / total).max(2.0).round(), BAR).radius(3.0).background(*color).key(*key)).collect();
    let used: f64 = children.iter().filter_map(|c| c.style.width).sum();
    children.push(Node::default().size((room - used).max(0.0), BAR).radius(3.0).background(rx.theme.surface).key("rest"));
    Node::row(children).gap(2.0).height(BAR).key("bar")
}

/// e.g. 4,3% for 0.043
fn pct(f: f64) -> String {
    format!("{:.1}%", f * 100.0).replace('.', t(",", "."))
}

/// e.g. 850 MB or 3,2 GB, in binary units as Activity Monitor counts memory.
fn size(bytes: f64) -> String {
    let mb = bytes / 1024.0 / 1024.0;
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0).replace('.', t(",", ".")) } else { format!("{} MB", mb.round()) }
}

/// e.g. 494 GB, 5,9 GB or 2 TB, in decimal units as the Finder counts disks.
fn disk_size(bytes: f64) -> String {
    let (v, unit) = match bytes {
        1e12.. => (bytes / 1e12, "TB"),
        1e9.. => (bytes / 1e9, "GB"),
        _ => (bytes / 1e6, "MB"),
    };
    let s = if v < 10.0 && unit != "MB" { format!("{v:.1}") } else { format!("{v:.0}") };
    // 2 TB, not 2,0 TB
    let s = s.strip_suffix(".0").unwrap_or(&s);
    format!("{} {unit}", s.replace('.', t(",", ".")))
}

/// The ticks of all the CPUs since boot: user, system, idle, nice. They are 32 bits and wrap
/// around after about a month, hence the wrapping deltas.
fn cpu_ticks() -> Option<[u32; 4]> {
    unsafe {
        let mut info: libc::host_cpu_load_info = std::mem::zeroed();
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        #[allow(deprecated)]
        let host = libc::mach_host_self();
        let r = libc::host_statistics(host, libc::HOST_CPU_LOAD_INFO, &mut info as *mut _ as *mut i32, &mut count);
        (r == 0).then(|| info.cpu_ticks.map(|t| t))
    }
}

/// The share of the time between two readings in user (with nice), system and idle.
fn cpu_time(old: [u32; 4], new: [u32; 4]) -> (f64, f64, f64) {
    let d: Vec<f64> = (0..4).map(|i| new[i].wrapping_sub(old[i]) as f64).collect();
    let total: f64 = d.iter().sum();
    if total <= 0.0 {
        return (0.0, 0.0, 1.0);
    }
    ((d[0] + d[3]) / total, d[1] / total, d[2] / total)
}

/// As Activity Monitor counts it: app memory (anonymous pages that can't be purged), wired and
/// compressed.
fn memory() -> Option<Memory> {
    unsafe {
        let mut s: libc::vm_statistics64 = std::mem::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        #[allow(deprecated)]
        let host = libc::mach_host_self();
        if libc::host_statistics64(host, libc::HOST_VM_INFO64, &mut s as *mut _ as *mut i32, &mut count) != 0 {
            return None;
        }
        let page = libc::sysconf(libc::_SC_PAGESIZE) as f64;
        let app = (s.internal_page_count as f64 - s.purgeable_count as f64) * page;
        let wired = s.wire_count as f64 * page;
        let compressed = s.compressor_page_count as f64 * page;
        Some(Memory { used: app + wired + compressed, app, wired, compressed })
    }
}

fn sysctl<T>(name: &std::ffi::CStr) -> Option<T> {
    unsafe {
        let mut v: T = std::mem::zeroed();
        let mut size = std::mem::size_of::<T>();
        (libc::sysctlbyname(name.as_ptr(), &mut v as *mut T as *mut _, &mut size, std::ptr::null_mut(), 0) == 0).then_some(v)
    }
}

fn pressure() -> Pressure {
    match sysctl::<i32>(c"kern.memorystatus_vm_pressure_level") {
        Some(2) => Pressure::Warning,
        Some(4) => Pressure::Critical,
        _ => Pressure::Normal,
    }
}

fn swap_used() -> f64 {
    sysctl::<libc::xsw_usage>(c"vm.swapusage").map(|s| s.xsu_used as f64).unwrap_or(0.0)
}

/// The utilization of the busiest GPU, for the Macs with two.
fn gpu_utilization() -> Option<u32> {
    iokit::services(c"IOAccelerator")
        .iter()
        .filter_map(|g| g.dict("PerformanceStatistics"))
        .filter_map(|s| iokit::number(&s, &NSString::from_str("Device Utilization %")))
        .max()
        .map(|v| v.clamp(0, 100) as u32)
}

/// e.g. "Apple M4 Pro · 16 core"
fn gpu_name(model: String, cores: Option<i64>) -> String {
    let core = t("core", "cores");
    match cores {
        Some(n) if !model.is_empty() => format!("{model} · {n} {core}"),
        Some(n) => format!("{n} {core}"),
        None => model,
    }
}

/// The disks the Finder shows that hold files: the startup disk, then by name the local volumes
/// that aren't hidden or read-only (external drives, not disk images). The mount table comes with
/// `MNT_NOWAIT`, so a network share that doesn't answer can't hang it, and network shares are
/// left out before anything asks them for their sizes.
fn volumes() -> Vec<Volume> {
    let mut mounts: Vec<libc::statfs> = Vec::new();
    unsafe {
        let n = libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT);
        if n <= 0 {
            return Vec::new();
        }
        // Room for a few more, in case one is mounted in between
        mounts.resize(n as usize + 8, std::mem::zeroed());
        let n = libc::getfsstat(mounts.as_mut_ptr(), (mounts.len() * std::mem::size_of::<libc::statfs>()) as i32, libc::MNT_NOWAIT);
        mounts.truncate(n.max(0) as usize);
    }
    let mut out: Vec<Volume> = objc2::rc::autoreleasepool(|_| {
        mounts
            .iter()
            .filter_map(|m| {
                let path = unsafe { std::ffi::CStr::from_ptr(m.f_mntonname.as_ptr()) }.to_string_lossy().to_string();
                let flags = m.f_flags as i32;
                let shown = flags & libc::MNT_LOCAL != 0 && flags & (libc::MNT_DONTBROWSE | libc::MNT_RDONLY) == 0;
                // The startup disk is a read-only snapshot of the system: its sizes are those of the disk
                if path != "/" && !shown {
                    return None;
                }
                volume(&path).filter(|v| v.total > 0.0)
            })
            .collect()
    });
    out.sort_by(|a, b| (b.path == "/").cmp(&(a.path == "/")).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// A disk's name and sizes, as the Finder shows them.
fn volume(path: &str) -> Option<Volume> {
    use objc2_foundation::{
        NSURLVolumeAvailableCapacityForImportantUsageKey as IMPORTANT, NSURLVolumeAvailableCapacityKey as FREE, NSURLVolumeIsInternalKey as INTERNAL,
        NSURLVolumeLocalizedNameKey as NAME, NSURLVolumeTotalCapacityKey as TOTAL,
    };
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path), true);
    let keys = unsafe { [NAME, TOTAL, FREE, IMPORTANT, INTERNAL] };
    let values = url.resourceValuesForKeys_error(&NSArray::from_slice(&keys)).ok()?;
    let number = |key: &NSString| values.objectForKey(key).and_then(|v| v.downcast::<NSNumber>().ok());
    let bytes = |key: &NSString| number(key).map(|n| n.doubleValue()).unwrap_or(0.0);
    let name = values
        .objectForKey(unsafe { NAME })
        .and_then(|v| v.downcast::<NSString>().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.rsplit('/').find(|p| !p.is_empty()).unwrap_or(path).to_string());
    let free = bytes(unsafe { FREE });
    // What can be purged comes with the startup disk's file system; elsewhere it's just what's free
    let available = bytes(unsafe { IMPORTANT }).max(free);
    Some(Volume {
        name,
        path: path.to_string(),
        total: bytes(unsafe { TOTAL }),
        available,
        purgeable: available - free,
        internal: number(unsafe { INTERNAL }).is_some_and(|n| n.boolValue()),
    })
}

/// The GPU time of each process in ns, which the driver keeps in the connections each process
/// opened to it.
fn gpu_times() -> HashMap<i32, u64> {
    let mut times = HashMap::new();
    let key = NSString::from_str("accumulatedGPUTime");
    for gpu in iokit::services(c"IOAccelerator") {
        for client in gpu.children() {
            // "pid 407, WindowServer"
            let Some(pid) = client.string("IOUserClientCreator").and_then(|c| c.strip_prefix("pid ")?.split(',').next()?.parse::<i32>().ok()) else {
                continue;
            };
            let Some(usage) = client.property("AppUsage") else { continue };
            let Ok(usage) = usage.downcast::<objc2_foundation::NSArray>() else { continue };
            let total: i64 = usage
                .iter()
                .filter_map(|u| u.downcast::<objc2_foundation::NSDictionary>().ok())
                .map(|d| unsafe { objc2::rc::Retained::cast_unchecked::<iokit::Dict>(d) })
                .filter_map(|d| iokit::number(&d, &key))
                .sum();
            *times.entry(pid).or_insert(0) += total.max(0) as u64;
        }
    }
    times
}

fn pids() -> Vec<i32> {
    unsafe {
        let n = libc::proc_listallpids(std::ptr::null_mut(), 0);
        let mut pids = vec![0i32; n.max(0) as usize + 64];
        let n = libc::proc_listallpids(pids.as_mut_ptr() as *mut _, (pids.len() * 4) as i32);
        pids.truncate(n.max(0) as usize);
        pids
    }
}

/// The percentage of a core each process used lately, from `ps`, the only one that sees them all.
fn process_cpu() -> HashMap<i32, f64> {
    let out = runtime::run("/bin/ps", &["-Ao", "pid=,pcpu="], None, std::time::Duration::from_secs(3));
    out.stdout
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?.parse().ok()?, f.next()?.replace(',', ".").parse().ok()?))
        })
        .collect()
}

/// The memory of each of the user's processes, as Activity Monitor counts it.
fn process_memory() -> HashMap<i32, f64> {
    let mut out = HashMap::new();
    for pid in pids() {
        unsafe {
            let mut info: libc::rusage_info_v2 = std::mem::zeroed();
            if libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, &mut info as *mut _ as *mut _) == 0 {
                out.insert(pid, info.ri_phys_footprint as f64);
            }
        }
    }
    out
}

/// The app a process belongs to: the outermost .app of its path (the helpers of Chrome count as
/// Chrome), or else its name.
fn owner(pid: i32, cache: &mut HashMap<String, App>) -> Option<App> {
    let mut buf = vec![0u8; 4 * libc::PATH_MAX as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut _, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    let path = String::from_utf8_lossy(&buf[..n as usize]).to_string();
    if let Some(app) = cache.get(&path) {
        return Some(app.clone());
    }
    let app = match path.find(".app/") {
        Some(i) => {
            let dir = &path[..i + 4];
            let ns = NSString::from_str(dir);
            App {
                bundle: NSBundle::bundleWithPath(&ns).and_then(|b| b.bundleIdentifier()).map(|s| s.to_string()).unwrap_or_default(),
                // As the Finder shows it, e.g. Monitoraggio Attività
                name: NSFileManager::defaultManager().displayNameAtPath(&ns).to_string(),
            }
        }
        None => App { bundle: String::new(), name: path.rsplit('/').next().unwrap_or(&path).to_string() },
    };
    cache.insert(path, app.clone());
    Some(app)
}

/// The apps with the largest sums of the values of their processes, rounded to `unit`.
fn top(values: &HashMap<i32, f64>, unit: f64, cache: &mut HashMap<String, App>) -> Vec<(App, f64)> {
    let mut sums: HashMap<String, (App, f64)> = HashMap::new();
    for (pid, v) in values {
        if *v <= 0.0 {
            continue;
        }
        if let Some(app) = owner(*pid, cache) {
            let key = format!("{}\u{1f}{}", app.bundle, app.name);
            sums.entry(key).or_insert((app, 0.0)).1 += v;
        }
    }
    let mut list: Vec<(App, f64)> = sums.into_values().filter(|(_, v)| (v / unit).round() > 0.0).collect();
    list.sort_by(|a, b| b.1.total_cmp(&a.1));
    list.truncate(TOP);
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_sizes() {
        // The tests run in the language of the Mac: compare without the decimal separator
        let plain = |b: f64| disk_size(b).replace(',', ".");
        assert_eq!(plain(494.38e9), "494 GB");
        assert_eq!(plain(5.9e9), "5.9 GB");
        assert_eq!(plain(2.0e12), "2 TB");
        assert_eq!(plain(12.4e12), "12 TB");
        assert_eq!(plain(840e6), "840 MB");
    }

    #[test]
    fn startup_disk() {
        let v = volumes();
        let root = v.first().expect("the startup disk");
        assert_eq!(root.path, "/");
        assert!(root.total > 0.0 && root.available > 0.0 && root.available <= root.total);
        assert!(root.purgeable >= 0.0 && root.used() > 0.0);
        assert!(v.iter().all(|v| !v.path.starts_with("/System/Volumes")));
    }
}
