//! Docker: the running containers, from the Engine API on its Unix socket, with no `docker` CLI.
//!
//! - Bar: Docker Desktop's whale in Docker's blue (a box without Docker Desktop) with how many
//!   containers run, paused ones too as in `docker ps`: gray when none, red when one fails its
//!   health check or keeps restarting. Hidden while Docker isn't running.
//! - Popup: the engine (`Docker Desktop`, `OrbStack` or `Docker`, and `Engine 29.0.1`), its CPUs,
//!   memory and images; `Container` with how many run and how many are stopped; then the running
//!   containers by name, so that those of a Compose project stay together, at most ten (the others
//!   counted): an icon for the state (green running, yellow paused or starting its health check,
//!   red failing it or restarting), the name, the CPU and memory it uses, and under it the image,
//!   the published ports and how long it's been up (`nginx:1.27-alpine · :8080 · da 2 h`) or its
//!   state. `Apri Docker Desktop` opens the Dashboard.
//! - Data: `/info` and `/containers/json` (and each one shown, for when it started), read again a
//!   second after the last of a burst of `/events` (start, die, pause, unpause, health, rename),
//!   so `docker compose up` updates the bar once. While Docker is off, its socket is tried every
//!   5 seconds, in-process. CPU and memory come from `/containers/<id>/stats` only while the popup
//!   is open, every few seconds (each takes a second or two: until then the last ones show).
//!   Docker Desktop, OrbStack, colima, Rancher Desktop and `DOCKER_HOST=unix://…` all work.

use std::collections::HashMap;
use std::io::BufRead;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::net;
use crate::runtime;
use crate::ui::Node;

const WHALE: &str = "/Applications/Docker.app#Whale";
const MAX: usize = 10;
const POLL: Duration = Duration::from_secs(5);
const NAME_WIDTH: f64 = 170.0;
const DETAILS_WIDTH: f64 = 290.0;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Engine {
    os: String,
    version: String,
    cpus: u64,
    /// Bytes.
    memory: f64,
    images: u64,
    stopped: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Container {
    id: String,
    name: String,
    image: String,
    /// running, paused, restarting…
    state: String,
    /// healthy, unhealthy, starting, or empty without a health check.
    health: String,
    ports: Vec<u16>,
    /// Seconds since 1970 (in made-up data, seconds ago).
    started: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Snapshot {
    engine: Engine,
    /// All the running ones, by name; only the first `MAX` have `started`.
    containers: Vec<Container>,
}

/// Made-up data (see `mock`): the engine, the containers and what they use, by name.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    engine: Option<Engine>,
    containers: Vec<Container>,
    /// Name → [CPU %, MB].
    usage: HashMap<String, (f64, f64)>,
}

/// The stream of events: closed when the widget goes (a reload), which ends its thread.
struct Closer(UnixStream);

impl Drop for Closer {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

pub struct Docker {
    /// None while Docker isn't running.
    snapshot: Option<Snapshot>,
    /// CPU % and MB, by container name.
    usage: HashMap<String, (f64, f64)>,
    socket: Option<PathBuf>,
    checked: bool,
    events: Option<Closer>,
    /// Events in a burst: the last one reads again.
    burst: u64,
    measuring: bool,
    mock: bool,
}

/// The sockets of the engines, the one of `DOCKER_HOST` first.
fn sockets() -> Vec<PathBuf> {
    let home = crate::config::home();
    let mut list: Vec<PathBuf> = std::env::var("DOCKER_HOST").ok().and_then(|h| h.strip_prefix("unix://").map(PathBuf::from)).into_iter().collect();
    list.extend([
        home.join(".docker/run/docker.sock"),
        PathBuf::from("/var/run/docker.sock"),
        home.join(".orbstack/run/docker.sock"),
        home.join(".colima/default/docker.sock"),
        home.join(".rd/docker.sock"),
    ]);
    list
}

fn get(path: &Path, target: &str) -> Option<Value> {
    let response = net::unix_get(path, target, Some(Duration::from_secs(10))).ok()?;
    if response.status != 200 {
        return None;
    }
    serde_json::from_str(&response.text().ok()?).ok()
}

/// `ghcr.io/org/app:1.2@sha256:…` → `app:1.2`; an image without a tag left only its id.
fn short_image(image: &str) -> String {
    let image = image.split('@').next().unwrap_or(image);
    if let Some(id) = image.strip_prefix("sha256:") {
        return id.chars().take(12).collect();
    }
    image.rsplit('/').next().unwrap_or(image).to_string()
}

/// The engine and the running containers.
fn snapshot(path: &Path) -> Option<Snapshot> {
    let info = get(path, "/info")?;
    let engine = Engine {
        os: info["OperatingSystem"].as_str().unwrap_or_default().to_string(),
        version: info["ServerVersion"].as_str().unwrap_or_default().to_string(),
        cpus: info["NCPU"].as_u64().unwrap_or(0),
        memory: info["MemTotal"].as_f64().unwrap_or(0.0),
        images: info["Images"].as_u64().unwrap_or(0),
        stopped: info["ContainersStopped"].as_u64().unwrap_or(0),
    };
    let list = get(path, "/containers/json")?;
    let mut containers: Vec<Container> = list
        .as_array()?
        .iter()
        .map(|c| {
            let status = c["Status"].as_str().unwrap_or_default();
            let health = if status.contains("(unhealthy)") {
                "unhealthy"
            } else if status.contains("(health: starting)") {
                "starting"
            } else if status.contains("(healthy)") {
                "healthy"
            } else {
                ""
            };
            let mut ports: Vec<u16> = c["Ports"].as_array().into_iter().flatten().filter_map(|p| p["PublicPort"].as_u64()).map(|p| p as u16).collect();
            ports.sort();
            ports.dedup();
            Container {
                id: c["Id"].as_str().unwrap_or_default().to_string(),
                name: c["Names"][0].as_str().unwrap_or_default().trim_start_matches('/').to_string(),
                image: short_image(c["Image"].as_str().unwrap_or_default()),
                state: c["State"].as_str().unwrap_or_default().to_string(),
                health: health.to_string(),
                ports,
                started: None,
            }
        })
        .collect();
    containers.sort_by(|a, b| a.name.cmp(&b.name));
    for c in containers.iter_mut().take(MAX) {
        c.started = get(path, &format!("/containers/{}/json", c.id)).and_then(|i| i["State"]["StartedAt"].as_str().and_then(crate::time::iso8601));
    }
    Some(Snapshot { engine, containers })
}

/// CPU % (of one core, as `docker stats`) and memory in MB of a container: the engine samples it
/// twice, a second apart.
fn stats(path: &Path, id: &str) -> Option<(f64, f64)> {
    let s = get(path, &format!("/containers/{id}/stats?stream=false"))?;
    let num = |v: &Value| v.as_f64().unwrap_or(0.0);
    let cpu = num(&s["cpu_stats"]["cpu_usage"]["total_usage"]) - num(&s["precpu_stats"]["cpu_usage"]["total_usage"]);
    let system = num(&s["cpu_stats"]["system_cpu_usage"]) - num(&s["precpu_stats"]["system_cpu_usage"]);
    let cpus = s["cpu_stats"]["online_cpus"].as_f64().filter(|n| *n > 0.0).unwrap_or(1.0);
    let percent = if system > 0.0 && cpu > 0.0 { cpu / system * cpus * 100.0 } else { 0.0 };
    // Without the page cache, as `docker stats`: cgroup v2, then v1
    let m = &s["memory_stats"];
    let cache = m["stats"]["inactive_file"].as_f64().or(m["stats"]["total_inactive_file"].as_f64()).unwrap_or(0.0);
    let used = (num(&m["usage"]) - cache).max(0.0);
    Some((percent, used / 1_048_576.0))
}

fn decimal(v: f64) -> String {
    let s = format!("{v:.1}");
    if crate::i18n::italian() { s.replace('.', ",") } else { s }
}

/// `850 MB`, `3,2 GB`.
fn size(mb: f64) -> String {
    if mb >= 1024.0 { format!("{} GB", decimal(mb / 1024.0)) } else { format!("{mb:.0} MB") }
}

/// `1 attivo`, `3 attivi`.
fn count(n: u64, one: (&str, &str), many: (&str, &str)) -> String {
    let (it, en) = if n == 1 { one } else { many };
    format!("{n} {}", t(it, en))
}

/// `da 2 h`: the largest unit only.
fn up_for(started: f64) -> String {
    let up = crate::time::now() - started;
    let span = if up >= 86_400.0 {
        format!("{:.0} {}", (up / 86_400.0).floor(), t("g", "d"))
    } else if up >= 3600.0 {
        format!("{:.0} h", (up / 3600.0).floor())
    } else if up >= 60.0 {
        format!("{:.0} min", (up / 60.0).floor())
    } else {
        return t("appena avviato", "just started").into();
    };
    t(&format!("da {span}"), &format!("up {span}")).to_string()
}

impl Docker {
    pub fn new(_options: &Options) -> Docker {
        let mock = crate::mock::load::<Mock>("docker");
        let mut d = Docker { snapshot: None, usage: HashMap::new(), socket: None, checked: false, events: None, burst: 0, measuring: false, mock: mock.is_some() };
        if let Some(m) = mock {
            let now = crate::time::now();
            let mut containers = m.containers;
            for c in &mut containers {
                c.started = c.started.map(|ago| now - ago);
            }
            containers.sort_by(|a, b| a.name.cmp(&b.name));
            d.snapshot = m.engine.map(|engine| Snapshot { engine, containers });
            d.usage = m.usage;
            d.checked = true;
        }
        d
    }

    /// Finds the engine, reads it, then follows its events until it stops; again and again, until
    /// a reload.
    fn watch(&mut self, cx: &mut Cx) {
        let handle = cx.handle::<Docker>();
        runtime::spawn("docker", move || {
            while handle.alive() {
                let since = crate::time::now() as i64;
                let found = sockets().into_iter().find_map(|path| snapshot(&path).map(|s| (path, s)));
                let Some((path, snap)) = found else {
                    handle.send(|w, cx| w.update(None, None, cx));
                    std::thread::sleep(POLL);
                    continue;
                };
                let socket = path.clone();
                handle.send(move |w, cx| w.update(Some(socket), Some(snap), cx));
                let filters = r#"{"type":["container"],"event":["start","die","pause","unpause","health_status","rename","destroy"]}"#;
                let encoded: String = filters.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'_' { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
                let Ok(response) = net::unix_get(&path, &format!("/events?since={since}&filters={encoded}"), None) else {
                    std::thread::sleep(POLL);
                    continue;
                };
                let closer = response.stream.try_clone().ok();
                handle.send(move |w, _| w.events = closer.map(Closer));
                for line in response.body.lines() {
                    if line.is_err() || !handle.alive() {
                        break;
                    }
                    handle.send(|w, cx| w.changed(cx));
                }
                // Docker stopped, or a reload closed the stream
                handle.send(|w, cx| {
                    w.events = None;
                    w.changed(cx);
                });
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    fn update(&mut self, socket: Option<PathBuf>, snapshot: Option<Snapshot>, cx: &mut Cx) {
        let changed = !self.checked || self.snapshot != snapshot;
        self.checked = true;
        self.socket = socket;
        if snapshot.is_none() {
            self.usage.clear();
            if cx.popup_open {
                cx.close_popup();
            }
        }
        self.snapshot = snapshot;
        if changed {
            cx.redraw();
        }
    }

    /// An event: read again a second after the last one of a burst.
    fn changed(&mut self, cx: &mut Cx) {
        self.burst += 1;
        let burst = self.burst;
        cx.after::<Docker>(1.0, move |w, cx| {
            if w.burst != burst {
                return;
            }
            let Some(path) = w.socket.clone() else { return };
            cx.background::<Docker, Option<Snapshot>>(move || snapshot(&path), |w, snap, cx| {
                let socket = snap.as_ref().and(w.socket.clone());
                w.update(socket, snap, cx);
            });
        });
    }

    /// CPU and memory of the containers shown, while the popup is open.
    fn measure(&mut self, cx: &mut Cx) {
        if self.mock || self.measuring || !cx.popup_open {
            return;
        }
        let (Some(path), Some(snap)) = (self.socket.clone(), &self.snapshot) else { return };
        let shown: Vec<(String, String)> = snap.containers.iter().take(MAX).map(|c| (c.id.clone(), c.name.clone())).collect();
        if shown.is_empty() {
            return;
        }
        self.measuring = true;
        cx.background::<Docker, HashMap<String, (f64, f64)>>(
            move || {
                std::thread::scope(|scope| {
                    let jobs: Vec<_> = shown.iter().map(|(id, name)| (name, scope.spawn(|| stats(&path, id)))).collect();
                    jobs.into_iter().filter_map(|(name, job)| Some((name.clone(), job.join().ok()??))).collect()
                })
            },
            |w, usage, cx| {
                w.measuring = false;
                w.usage.extend(usage);
                cx.redraw();
                if cx.popup_open {
                    cx.after::<Docker>(1.0, |w, cx| w.measure(cx));
                }
            },
        );
    }
}

impl Widget for Docker {
    fn start(&mut self, cx: &mut Cx) {
        if !self.mock {
            self.watch(cx);
        }
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let snap = self.snapshot.as_ref()?;
        let n = snap.containers.len();
        let failing = snap.containers.iter().any(|c| c.state == "restarting" || c.health == "unhealthy");
        let color = if failing { th.red } else if n == 0 { th.subtext } else { th.text };
        let icon = if Path::new("/Applications/Docker.app").exists() {
            Node::image(WHALE, 0.0, 12.0, Some(Rgba::parse("#2496ed").unwrap_or(th.primary)))
        } else {
            Node::symbol("shippingbox.fill", th.icon_size, Weight::Semibold, th.text)
        };
        Some(rx.pill(vec![icon.key("icon"), Node::text(n.to_string(), th.font(), color).key("count")]).click(rx.toggle_popup()))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let snap = self.snapshot.as_ref()?;
        let e = &snap.engine;
        let bold = th.font();
        let small = th.small_font();
        let title = match e.os.as_str() {
            os @ ("Docker Desktop" | "OrbStack") => os,
            _ => "Docker",
        };
        let line = |left: Node, right: Option<Node>, key: &str| {
            let mut c = vec![left, Node::spacer()];
            c.extend(right);
            Node::row(c).gap(16.0).padding(0.0, 8.0, 0.0, 8.0).key(key)
        };
        let mut rows = vec![Node::default().height(4.0).key("top")];
        rows.push(line(Node::text(title, bold.clone(), th.text), Some(Node::text(format!("Engine {}", e.version), small.clone(), th.subtext)), "engine"));
        let resources = format!(
            "{} · {} · {}",
            count(e.cpus, ("CPU", "CPU"), ("CPU", "CPUs")),
            t(&format!("{} di memoria", size(e.memory / 1_048_576.0)), &format!("{} of memory", size(e.memory / 1_048_576.0))),
            count(e.images, ("immagine", "image"), ("immagini", "images")),
        );
        rows.push(Node::text(resources, small.clone(), th.subtext).padding(2.0, 8.0, 0.0, 8.0).key("resources"));
        let mut summary = count(snap.containers.len() as u64, ("attivo", "running"), ("attivi", "running"));
        if e.stopped > 0 {
            summary.push_str(&format!(" · {}", count(e.stopped, ("fermo", "stopped"), ("fermi", "stopped"))));
        }
        rows.push(Node::default().height(10.0).key("gap"));
        rows.push(line(Node::text("Container", bold.clone(), th.text), Some(Node::text(summary, small.clone(), th.subtext)), "title"));
        rows.push(Node::default().height(2.0).key("title-gap"));
        for c in snap.containers.iter().take(MAX) {
            let (symbol, color, state) = match (c.state.as_str(), c.health.as_str()) {
                ("paused", _) => ("pause.circle.fill", th.yellow, Some(t("in pausa", "paused"))),
                ("restarting", _) => ("arrow.clockwise.circle.fill", th.red, Some(t("in riavvio", "restarting"))),
                (_, "unhealthy") => ("exclamationmark.circle.fill", th.red, Some(t("health check fallito", "health check failing"))),
                (_, "starting") => ("circle.fill", th.yellow, Some(t("in avvio", "starting"))),
                _ => ("circle.fill", th.green, None),
            };
            let usage = self.usage.get(&c.name).map(|(cpu, mb)| format!("{}% · {}", decimal(*cpu), size(*mb))).unwrap_or_default();
            rows.push(
                Node::row(vec![
                    Node::row(vec![Node::symbol(symbol, 11.0, Weight::Semibold, color)]).width(14.0).key("state"),
                    Node::text(&c.name, bold.clone(), th.text).max_width(NAME_WIDTH).key("name"),
                    Node::spacer(),
                    Node::text(usage, bold.clone(), th.subtext).key("usage"),
                ])
                .gap(8.0)
                .padding(8.0, 8.0, 0.0, 8.0)
                .key(format!("c-{}", c.name)),
            );
            let mut details = vec![c.image.clone()];
            if !c.ports.is_empty() {
                details.push(c.ports.iter().map(|p| format!(":{p}")).collect::<Vec<_>>().join(", "));
            }
            details.extend(state.map(String::from).or_else(|| c.started.map(up_for)));
            rows.push(Node::text(details.join(" · "), small.clone(), th.subtext).max_width(DETAILS_WIDTH + 30.0).padding(2.0, 8.0, 0.0, 30.0).key(format!("d-{}", c.name)));
        }
        match snap.containers.len() {
            0 => rows.push(Node::text(t("Nessun container in esecuzione", "No running containers"), small.clone(), th.subtext).padding(6.0, 8.0, 0.0, 8.0).key("more")),
            n if n > MAX => rows.push(Node::text(t(&format!("e altri {}", n - MAX), &format!("and {} more", n - MAX)), small.clone(), th.subtext).padding(6.0, 8.0, 0.0, 30.0).key("more")),
            _ => {}
        }
        if title == "Docker Desktop" && Path::new("/Applications/Docker.app").exists() {
            rows.push(Node::default().height(8.0).key("open-gap"));
            rows.push(rx.row_button(vec![Node::text(t("Apri Docker Desktop", "Open Docker Desktop"), bold.clone(), th.primary)], vec![], rx.action("open")).key("open"));
        }
        rows.push(Node::default().height(4.0).key("bottom"));
        Some(rx.popup(340.0, rows).padding(4.0, 4.0, 4.0, 4.0).gap(0.0))
    }

    fn ready(&self) -> bool {
        self.checked
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        if name == "open" {
            cx.close_popup();
            if !self.mock {
                runtime::open_app("com.docker.docker");
            }
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::PopupOpened => self.measure(cx),
            Event::Wake => self.changed(cx),
            Event::Trigger { name, .. } if name == "docker_update" => self.changed(cx),
            _ => {}
        }
    }

    crate::any_widget!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images() {
        assert_eq!(short_image("ghcr.io/org/app:1.2@sha256:abc"), "app:1.2");
        assert_eq!(short_image("postgres:16"), "postgres:16");
        assert_eq!(short_image("sha256:0123456789abcdef0123"), "0123456789ab");
        assert_eq!(size(512.0), "512 MB");
        assert!(size(3276.8).starts_with('3'));
    }
}
