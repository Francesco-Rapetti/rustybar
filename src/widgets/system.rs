//! CPU, GPU and memory usage, read every 2 seconds in-process and drawn only when a value changes.
//! CPU and GPU turn yellow from 75% and red from 90%; the memory has the color of its pressure, as
//! in Activity Monitor: macOS keeps the memory nearly full with caches and compressed pages, so the
//! percentage alone says little. The GPU hides when macOS doesn't report it, CPU and GPU in the
//! compact form.
//! The popup has, for each, a bar, the details and the apps that use it most, then a row that opens
//! Activity Monitor. The lists are read only while it is open: the CPU of each process from `ps`,
//! which is setuid and sees the system processes too, the GPU time from the IORegistry and the
//! memory from libproc (only the user's processes: the others are closed to it).
//! Options: `warning` (75) and `critical` (90).
//! With made-up data (see `mock`) the numbers, the GPU and the lists come from `system.json`.

use std::collections::HashMap;
use std::time::Instant;

use objc2_foundation::{NSBundle, NSFileManager, NSProcessInfo, NSString};
use serde::Deserialize;

use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::iokit;
use crate::runtime;
use crate::ui::{Kind, Node};

const INTERVAL: f64 = 2.0;
const TOP: usize = 5;
const NAME_WIDTH: f64 = 190.0;
const VALUE_WIDTH: f64 = 48.0;

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

/// The popup's details, in fractions and bytes.
#[derive(Clone, Debug, Default)]
struct Details {
    user: f64,
    system: f64,
    idle: f64,
    memory: Memory,
    swap: f64,
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

/// Made-up data (see `mock`): percentages, fractions for user, system and idle, sizes in GB, and
/// the lists as `[name, bundle id, value]` (a percentage, or GB for the memory).
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    cpu: u32,
    gpu: Option<u32>,
    ram: u32,
    pressure: Pressure,
    total: f64,
    gpu_model: String,
    gpu_cores: Option<i64>,
    user: f64,
    system: f64,
    idle: f64,
    memory: Memory,
    swap: f64,
    cpu_top: Vec<(String, String, f64)>,
    gpu_top: Vec<(String, String, f64)>,
    memory_top: Vec<(String, String, f64)>,
}

pub struct System {
    warning: u32,
    critical: u32,
    values: Values,
    ticks: Option<[u32; 4]>,
    loaded: bool,
    total: f64,
    gpu_model: String,
    /// While the popup is open: the details, and the GPU time of each process at the last reading.
    details: Option<Details>,
    gpu_times: Option<(HashMap<i32, u64>, Instant)>,
    reading: bool,
    /// The details of the made-up data, always shown.
    mock: Option<Details>,
}

impl System {
    pub fn new(options: &Options) -> System {
        let mut s = System {
            warning: options.f64("warning").unwrap_or(75.0) as u32,
            critical: options.f64("critical").unwrap_or(90.0) as u32,
            values: Values::default(),
            ticks: None,
            loaded: false,
            total: NSProcessInfo::processInfo().physicalMemory() as f64,
            gpu_model: String::new(),
            details: None,
            gpu_times: None,
            reading: false,
            mock: None,
        };
        if let Some(m) = crate::mock::load::<Mock>("system") {
            const GB: f64 = 1024.0 * 1024.0 * 1024.0;
            let list = |l: Vec<(String, String, f64)>, unit: f64| l.into_iter().map(|(name, bundle, v)| (App { bundle, name }, v * unit)).collect();
            s.values = Values { cpu: Some(m.cpu), gpu: m.gpu, ram: Some(m.ram), pressure: m.pressure };
            s.total = m.total * GB;
            s.gpu_model = gpu_name(m.gpu_model, m.gpu_cores);
            let memory = Memory { used: m.memory.used * GB, app: m.memory.app * GB, wired: m.memory.wired * GB, compressed: m.memory.compressed * GB };
            s.mock = Some(Details {
                user: m.user,
                system: m.system,
                idle: m.idle,
                memory,
                swap: m.swap * GB,
                cpu_top: list(m.cpu_top, 1.0),
                gpu_top: list(m.gpu_top, 1.0),
                memory_top: list(m.memory_top, GB),
            });
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
        if values != self.values {
            self.values = values;
            cx.redraw();
        }
        // The first reading has no CPU yet: the bar waits for the second, a moment later
        if time.is_some() {
            self.loaded = true;
        }
        if cx.popup_open && !self.reading
            && let (Some(time), Some(memory)) = (time, memory) {
                self.read_details(time, memory, cx);
            }
    }

    /// The lists of the popup, on a worker thread: `ps` and libproc take a few milliseconds.
    fn read_details(&mut self, time: (f64, f64, f64), memory: Memory, cx: &mut Cx) {
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
                    user: time.0,
                    system: time.1,
                    idle: time.2,
                    memory,
                    swap: swap_used(),
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

    fn section(&self, rx: &Rx, key: &str, title: &str, value: u32, color: Rgba, bar: Rgba, details: Vec<String>, apps: &[(App, f64)], format: fn(f64) -> String) -> Vec<Node> {
        let th = rx.theme;
        let mut rows = vec![
            Node::row(vec![Node::text(title, th.font(), th.text), Node::spacer(), Node::text(format!("{value}%"), th.font(), color)])
                .padding(if key == "cpu" { 6.0 } else { 14.0 }, 8.0, 6.0, 8.0)
                .key(format!("{key}-title")),
            Node::new(Kind::Progress { value: value as f64 / 100.0, color: bar, track: th.surface })
                .height(6.0)
                .radius(3.0)
                .padding(0.0, 8.0, 0.0, 8.0)
                .key(format!("{key}-bar")),
        ];
        for (i, d) in details.iter().enumerate() {
            rows.push(Node::text(d, th.small_font(), th.subtext).padding(if i == 0 { 6.0 } else { 2.0 }, 8.0, 0.0, 8.0).key(format!("{key}-d{i}")));
        }
        if !apps.is_empty() {
            rows.push(Node::default().height(4.0).key(format!("{key}-gap")));
        }
        for (app, v) in apps {
            let icon = if app.bundle.is_empty() { Node::default().size(16.0, 16.0) } else { Node::app_icon(&app.bundle, 16.0) };
            rows.push(
                Node::row(vec![
                    icon,
                    Node::text(&app.name, th.font(), th.text).max_width(NAME_WIDTH),
                    Node::spacer(),
                    Node::row(vec![Node::text(format(*v), th.font(), th.subtext)]).min_width(VALUE_WIDTH).justify(crate::ui::Justify::End),
                ])
                .gap(8.0)
                .height(22.0)
                .padding(0.0, 8.0, 0.0, 8.0)
                .key(format!("{key}-{}", app.name)),
            );
        }
        rows
    }
}

impl Widget for System {
    fn start(&mut self, cx: &mut Cx) {
        if self.mock.is_some() {
            return;
        }
        self.gpu_model = gpu_model();
        self.tick(cx);
        // The CPU needs two readings: the second one soon, so the bar doesn't wait 2 seconds
        cx.after::<System>(0.25, |w, cx| w.tick(cx));
        cx.every::<System>(INTERVAL, |w, cx| w.tick(cx));
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
        let details = self.details.as_ref().or(self.mock.as_ref());
        let d = details.cloned().unwrap_or_default();
        let has = details.is_some();
        let mut rows = Vec::new();
        let cpu = v.cpu.unwrap_or(0);
        let cpu_details = if has {
            vec![format!("{} {} · {} {} · {} {}", t("Sistema", "System"), pct(d.system), t("Utente", "User"), pct(d.user), t("Inattivo", "Idle"), pct(d.idle))]
        } else {
            vec![]
        };
        rows.extend(self.section(rx, "cpu", "CPU", cpu, self.level(rx, cpu, th.text), self.level(rx, cpu, th.primary), cpu_details, &d.cpu_top, |v| pct(v / 100.0)));
        if let Some(gpu) = v.gpu {
            let details = if self.gpu_model.is_empty() { vec![] } else { vec![self.gpu_model.clone()] };
            rows.extend(self.section(rx, "gpu", "GPU", gpu, self.level(rx, gpu, th.text), self.level(rx, gpu, th.primary), details, &d.gpu_top, |v| pct(v / 100.0)));
        }
        if let Some(ram) = v.ram {
            let pressure = match v.pressure {
                Pressure::Normal => t("normale", "normal"),
                Pressure::Warning => t("alta", "high"),
                Pressure::Critical => t("critica", "critical"),
            };
            let mut details = Vec::new();
            if has {
                let m = d.memory;
                let mut used = format!("{} {} {} GB", size(m.used), t("di", "of"), (self.total / 1024f64.powi(3)).round());
                if d.swap > 0.0 {
                    used += &format!(" · swap {}", size(d.swap));
                }
                details.push(format!("{used} · {} {pressure}", t("pressione", "pressure")));
                details.push(format!("App {} · wired {} · {} {}", size(m.app), size(m.wired), t("compressa", "compressed"), size(m.compressed)));
            }
            rows.extend(self.section(rx, "ram", t("Memoria", "Memory"), ram, self.pressure_color(rx, th.text), self.pressure_color(rx, th.primary), details, &d.memory_top, size));
        }
        rows.push(Node::default().height(8.0).key("gap"));
        rows.push(rx.row_button(vec![Node::text(t("Apri Monitoraggio Attività", "Open Activity Monitor"), th.font(), th.primary)], vec![], rx.action("activity")).key("activity"));
        // Fixed, so the updates every 2 seconds don't change its width
        let width = 8.0 + 16.0 + 8.0 + NAME_WIDTH + 8.0 + VALUE_WIDTH + 8.0 + 8.0;
        Some(rx.popup(width, rows).width(width).padding(4.0, 4.0, 4.0, 4.0).gap(0.0))
    }

    fn ready(&self) -> bool {
        self.loaded
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        if name == "activity" {
            cx.close_popup();
            if self.mock.is_some() {
                return;
            }
            runtime::open_app("com.apple.ActivityMonitor");
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
                cx.after::<System>(0.25, |w, cx| w.tick(cx));
            }
            Event::PopupClosed => {
                self.details = None;
                self.gpu_times = None;
            }
            Event::Wake => self.ticks = None,
            _ => {}
        }
    }

    crate::any_widget!();
}

/// e.g. 4,3% for 0.043
fn pct(f: f64) -> String {
    format!("{:.1}%", f * 100.0).replace('.', t(",", "."))
}

/// e.g. 850 MB or 3,2 GB
fn size(bytes: f64) -> String {
    let mb = bytes / 1024.0 / 1024.0;
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0).replace('.', t(",", ".")) } else { format!("{} MB", mb.round()) }
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

fn gpu_model() -> String {
    let gpus = iokit::services(c"IOAccelerator");
    let model = gpus.iter().find_map(|g| g.string("model")).unwrap_or_default();
    gpu_name(model, gpus.iter().find_map(|g| g.number("gpu-core-count")))
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
