//! Docker: the containers, from the Engine API on its Unix socket, with no `docker` CLI, and a
//! dashboard to start, stop, restart and remove them and to clean up what they leave behind.
//!
//! - Bar: Docker Desktop's whale in Docker's blue (a box without Docker Desktop) with how many
//!   containers run, paused ones too as in `docker ps`: gray when none, red when one fails its
//!   health check or keeps restarting. Hidden while Docker isn't running.
//! - Popup, a dashboard of tiles as `home`'s:
//!   - The engine, two tiles wide: the whale, `Docker Desktop` (`OrbStack`, `Docker`) and
//!     `Engine 29.0.1 · 8 CPU · 7,7 GB`; on the right the CPU and memory of all the containers.
//!   - The containers, all of them as in `docker ps -a`: those of each Compose project under its
//!     name, with how many run and ▶ ■ ↻ for the whole project (each container at once, without
//!     Compose's order), then the others under `Container`; two tiles in a row, by name, without
//!     the project's prefix. A tile has an icon in the color of the state (green running, yellow
//!     paused or starting its health check, red failing it, restarting or exited with an error,
//!     gray stopped), the CPU and memory it uses, the name, the image, then the published ports,
//!     links to `http://localhost:<port>`, and how long it's been up (`da 2 h`), or its state
//!     (`In pausa`, `Fermo`, `Uscito con codice 1`…). Last its buttons, dimmed where they'd do
//!     nothing: ▶ starts it (resumes it when paused), ■ stops it, ↻ restarts it and the bin removes
//!     it, running too, once the popup has asked. While a request is out the state says so
//!     (`Si ferma…`), and `Non riuscito` in red if it fails (Docker's reason goes in the log). When
//!     there are more than fit on the screen, they scroll.
//!   - `Spazio su disco`: images, containers, volumes and build cache, each with its size and what
//!     a clean-up would remove (`1,2 GB liberabili`, the containers by how many are stopped); a
//!     click asks, then cleans it up as `docker image prune -a`, `container prune`, `volume prune`
//!     (anonymous volumes only: named ones keep data) and `builder prune -a`, and says what it
//!     did. Under them `Pulisci reti…` and `Pulisci tutto…` (as `docker system prune -a`: stopped
//!     containers, unused networks and images, build cache; not volumes) work the same.
//!   - `Apri Docker Desktop` opens the Dashboard.
//!   - Every button that is only an icon, and every tile that does something, has a tip: what it
//!     does and to which containers (`Ferma db-1`, `Riavvia api-1, db-1 e web-1 di shop`).
//! - Asking, since a second click alone made it easy to remove what wasn't meant: nothing is
//!   removed at the first click. The popup shows, in place of the dashboard, what would go, by
//!   name and with its size, what stays, what's lost for good and how to get it back (`docker
//!   compose up -d in ~/Projects/shop`, or `docker run` again; images pulled or built again), as
//!   `Sheet`; a clean-up first reads the disk again, and its button waits until then, so the names
//!   are those of now. The button says what it does (`Elimina 3 container`) next to `Annulla`; a
//!   click on it within half a second of the question, as the second of a double click, does
//!   nothing. What goes is worked out as Docker does: `container prune` all but running, paused
//!   and restarting containers; `image prune -a` the images without containers (in `system
//!   prune`, after those, so the images of stopped containers go too); `network prune` the local
//!   networks without endpoints, which stopped containers don't have, so they're named as they
//!   won't start again. Closing the popup forgets the question.
//! - Data: `/info` and `/containers/json?all=1` (and each one up, for when it started), read again
//!   a second after the last of a burst of `/events` (create, start, die, pause, unpause, health,
//!   rename, destroy), so `docker compose up` updates the bar once, and at once when an action of
//!   the popup is done. While Docker is off, its socket is tried every 5 seconds, in-process. CPU
//!   and memory come from `/containers/<id>/stats` only while the popup is open, every few seconds
//!   (each takes a second or two: until then the last ones show); the disk from `/system/df` when
//!   it opens, when it asks about a clean-up and after an action, with `/networks` for what a
//!   clean-up of them removes. Docker Desktop, OrbStack, colima, Rancher Desktop and
//!   `DOCKER_HOST=unix://…` all work.

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
use crate::theme::Theme;
use crate::ui::{Align, Justify, Kind, Node, wrap};

const DESKTOP: &str = "/Applications/Docker.app";
const WHALE: &str = "/Applications/Docker.app#Whale";
const BLUE: &str = "#2496ed";
const PROJECT: &str = "com.docker.compose.project";
const PROJECT_DIR: &str = "com.docker.compose.project.working_dir";
/// The running containers with their uptime and usage: the others show only their state.
const MAX: usize = 24;
const POLL: Duration = Duration::from_secs(5);
/// For an action: a stop waits for the container's own timeout, a clean-up for the disk.
const SLOW: Duration = Duration::from_secs(300);
const MB: f64 = 1_048_576.0;
const TILE: f64 = 160.0;
const GAP: f64 = 8.0;
const WIDE: f64 = TILE * 2.0 + GAP;
/// Inside a tile's padding.
const INNER: f64 = TILE - 20.0;
const BUTTON: f64 = 31.0;
/// The bar and the popup without its containers: they scroll in what's left of the screen.
const AROUND: f64 = 460.0;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Engine {
    os: String,
    version: String,
    cpus: u64,
    /// Bytes.
    memory: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Container {
    id: String,
    name: String,
    /// Its Compose project, or empty.
    project: String,
    image: String,
    /// created, running, paused, restarting, removing, exited or dead.
    state: String,
    /// healthy, unhealthy, starting, or empty without a health check.
    health: String,
    /// Of an exited one.
    exit: Option<i64>,
    ports: Vec<u16>,
    /// Seconds since 1970 (in made-up data, seconds ago).
    started: Option<f64>,
    /// The folder of its Compose project, or empty.
    dir: String,
    /// Its image's id, as the images have it (none in made-up data: there the tags match).
    image_id: String,
    /// The networks it's on: their ids and names.
    networks: Vec<String>,
}

impl Container {
    /// Up, as in `docker ps`.
    fn up(&self) -> bool {
        matches!(self.state.as_str(), "running" | "paused" | "restarting")
    }

    /// Whether `op` does something to it.
    fn can(&self, op: Op) -> bool {
        match op {
            Op::Start => matches!(self.state.as_str(), "created" | "exited" | "paused"),
            Op::Stop | Op::Restart => self.up(),
            Op::Remove => self.state != "removing",
        }
    }

    /// What `container prune` removes: all but the running, paused and restarting ones.
    fn stopped(&self) -> bool {
        !self.up() && self.state != "removing"
    }

    /// Whether `image` is its image: by id, or by tag when no image has its id.
    fn uses(&self, image: &Image, images: &[Image]) -> bool {
        if images.iter().any(|i| i.id == self.image_id) {
            image.id == self.image_id
        } else {
            image.tags.iter().any(|tag| short_image(tag) == self.image)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Snapshot {
    engine: Engine,
    /// All of them, in the order of the popup (see `sort`); only the first `MAX` up have `started`.
    containers: Vec<Container>,
}

/// Bytes used and bytes that a clean-up frees, of each kind, and what's in them, for what a
/// clean-up would remove.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Disk {
    images: (f64, f64),
    containers: (f64, f64),
    volumes: (f64, f64),
    build: (f64, f64),
    /// All the images, and what all their layers take (`LayersSize`).
    image_list: Vec<Image>,
    layers: f64,
    /// How many volumes `volume prune` removes, and how many records of the build cache
    /// `builder prune -a` does.
    loose_volumes: usize,
    loose_build: usize,
    /// The networks that a clean-up may remove: local ones, not Docker's own.
    networks: Vec<Network>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Image {
    id: String,
    tags: Vec<String>,
    /// Bytes, and the bytes it shares with other images.
    size: f64,
    shared: f64,
    /// How many containers use it, as Docker counts them.
    containers: i64,
}

impl Image {
    /// What removing it alone frees.
    fn own(&self) -> f64 {
        (self.size - self.shared).max(0.0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Network {
    id: String,
    name: String,
}

/// What a clean-up did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Report {
    freed: f64,
    containers: usize,
    volumes: usize,
    networks: usize,
}

/// Made-up data (see `mock`): the engine, the containers, what they use (by name) and the disk.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    engine: Option<Engine>,
    containers: Vec<Container>,
    /// Name → [CPU %, MB].
    usage: HashMap<String, (f64, f64)>,
    disk: Option<Disk>,
}

/// A button of a container.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Start,
    Stop,
    Restart,
    Remove,
}

/// A clean-up: a kind of the disk, the networks, or all of them.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Prune {
    Images,
    Containers,
    Volumes,
    Build,
    Networks,
    All,
}

impl Prune {
    const ALL: [Prune; 6] = [Prune::Images, Prune::Containers, Prune::Volumes, Prune::Build, Prune::Networks, Prune::All];

    fn key(self) -> &'static str {
        match self {
            Prune::Images => "images",
            Prune::Containers => "containers",
            Prune::Volumes => "volumes",
            Prune::Build => "build",
            Prune::Networks => "networks",
            Prune::All => "all",
        }
    }

    /// The requests, in order: containers first, so that their images and networks are unused
    /// by then, as in `docker system prune`.
    fn targets(self) -> Vec<String> {
        match self {
            Prune::Containers => vec!["/containers/prune".into()],
            // As `-a`: every image without a container, not only the untagged ones
            Prune::Images => vec![format!("/images/prune?filters={}", query(r#"{"dangling":["false"]}"#))],
            // Anonymous volumes only, since Engine 23: named ones keep data
            Prune::Volumes => vec!["/volumes/prune".into()],
            Prune::Build => vec!["/build/prune?all=true".into()],
            Prune::Networks => vec!["/networks/prune".into()],
            Prune::All => [Prune::Containers, Prune::Networks, Prune::Images, Prune::Build].into_iter().flat_map(Prune::targets).collect(),
        }
    }
}

/// What a tile or a button says for a moment, over its state.
#[derive(Clone, Debug, PartialEq)]
enum Note {
    /// A request is out: what it does.
    Busy(&'static str),
    Failed,
    /// What a clean-up did.
    Done(String),
}

/// What the popup asks, in place of the dashboard, before removing something: what would go, by
/// name, what stays, and how to get it back.
#[derive(Clone, Debug, PartialEq)]
enum Ask {
    /// A container, by id.
    Remove(String),
    Prune(Prune),
}

/// The question, ready to draw.
#[derive(Clone, Debug, Default, PartialEq)]
struct Sheet {
    symbol: &'static str,
    title: String,
    /// The command it does as the CLI, or what it is.
    subtitle: String,
    /// What goes: an icon, what it is, its names and its size.
    goes: Vec<Goes>,
    /// What stays, what's lost, how to get it back: an icon and its color, and a sentence.
    notes: Vec<(&'static str, Hue, String)>,
    /// The button that does it; none when there's nothing to do.
    button: Option<String>,
    /// While what it removes is being measured, the button waits.
    measuring: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Goes {
    symbol: &'static str,
    what: String,
    names: Vec<String>,
    /// Bytes, when known.
    size: Option<f64>,
}

/// The color of a note, from the theme when drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hue {
    Keeps,
    Lost,
    Warning,
    Back,
    Info,
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
    /// None until the popup first opens.
    disk: Option<Disk>,
    socket: Option<PathBuf>,
    checked: bool,
    events: Option<Closer>,
    /// Events in a burst: the last one reads again.
    burst: u64,
    measuring: bool,
    /// `/system/df` is being read; `again` reads it once more after, for an action done meanwhile.
    sizing: bool,
    again: bool,
    /// By container id, or `prune:<kind>`.
    notes: HashMap<String, Note>,
    /// The question in the popup, and when it was asked.
    asking: Option<(Ask, f64)>,
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

fn get(path: &Path, target: &str, timeout: Duration) -> Option<Value> {
    let response = net::unix_get(path, target, Some(timeout)).ok()?;
    if response.status != 200 {
        return None;
    }
    serde_json::from_str(&response.text().ok()?).ok()
}

/// `method target` for an action: Docker's answer, or its reason when it refuses.
fn call(path: &Path, method: &str, target: &str) -> Result<Value, String> {
    let response = net::unix_request(path, method, target, Some(SLOW)).map_err(|e| e.to_string())?;
    let status = response.status;
    let body: Value = serde_json::from_str(&response.text().unwrap_or_default()).unwrap_or(Value::Null);
    // 304: started or stopped already
    if (200..300).contains(&status) || status == 304 {
        Ok(body)
    } else {
        Err(body["message"].as_str().map(String::from).unwrap_or_else(|| format!("HTTP {status}")))
    }
}

/// Percent-encoded, for a query string.
fn query(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'_' { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// `ghcr.io/org/app:1.2@sha256:…` → `app:1.2`; an image without a tag left only its id.
fn short_image(image: &str) -> String {
    let image = image.split('@').next().unwrap_or(image);
    if let Some(id) = image.strip_prefix("sha256:") {
        return id.chars().take(12).collect();
    }
    image.rsplit('/').next().unwrap_or(image).to_string()
}

/// The health check in the status of `/containers/json`: `Up 2 hours (healthy)`.
fn health(status: &str) -> &'static str {
    if status.contains("(unhealthy)") {
        "unhealthy"
    } else if status.contains("(health: starting)") {
        "starting"
    } else if status.contains("(healthy)") {
        "healthy"
    } else {
        ""
    }
}

/// `Exited (137) 2 hours ago` → 137.
fn exit_code(status: &str) -> Option<i64> {
    status.strip_prefix("Exited (")?.split(')').next()?.parse().ok()
}

/// The order of the popup: by project, those without one last, then by name.
fn sort(containers: &mut [Container]) {
    containers.sort_by(|a, b| (a.project.is_empty(), &a.project, &a.name).cmp(&(b.project.is_empty(), &b.project, &b.name)));
}

/// The engine and all the containers.
fn snapshot(path: &Path) -> Option<Snapshot> {
    let timeout = Duration::from_secs(10);
    let info = get(path, "/info", timeout)?;
    let engine = Engine {
        os: info["OperatingSystem"].as_str().unwrap_or_default().to_string(),
        version: info["ServerVersion"].as_str().unwrap_or_default().to_string(),
        cpus: info["NCPU"].as_u64().unwrap_or(0),
        memory: info["MemTotal"].as_f64().unwrap_or(0.0),
    };
    let list = get(path, "/containers/json?all=1", timeout)?;
    let mut containers: Vec<Container> = list
        .as_array()?
        .iter()
        .map(|c| {
            let status = c["Status"].as_str().unwrap_or_default();
            let mut ports: Vec<u16> = c["Ports"].as_array().into_iter().flatten().filter_map(|p| p["PublicPort"].as_u64()).map(|p| p as u16).collect();
            ports.sort();
            ports.dedup();
            Container {
                id: c["Id"].as_str().unwrap_or_default().to_string(),
                name: c["Names"][0].as_str().unwrap_or_default().trim_start_matches('/').to_string(),
                project: c["Labels"][PROJECT].as_str().unwrap_or_default().to_string(),
                image: short_image(c["Image"].as_str().unwrap_or_default()),
                state: c["State"].as_str().unwrap_or_default().to_string(),
                health: health(status).to_string(),
                exit: exit_code(status),
                ports,
                started: None,
                dir: c["Labels"][PROJECT_DIR].as_str().unwrap_or_default().to_string(),
                image_id: c["ImageID"].as_str().unwrap_or_default().to_string(),
                networks: c["NetworkSettings"]["Networks"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .flat_map(|(name, n)| [Some(name.clone()), n["NetworkID"].as_str().filter(|id| !id.is_empty()).map(String::from)])
                    .flatten()
                    .collect(),
            }
        })
        .collect();
    sort(&mut containers);
    for c in containers.iter_mut().filter(|c| c.up()).take(MAX) {
        c.started = get(path, &format!("/containers/{}/json", c.id), timeout).and_then(|i| i["State"]["StartedAt"].as_str().and_then(crate::time::iso8601));
    }
    Some(Snapshot { engine, containers })
}

/// CPU % (of one core, as `docker stats`) and memory in MB of a container: the engine samples it
/// twice, a second apart.
fn stats(path: &Path, id: &str) -> Option<(f64, f64)> {
    let s = get(path, &format!("/containers/{id}/stats?stream=false"), Duration::from_secs(10))?;
    let num = |v: &Value| v.as_f64().unwrap_or(0.0);
    let cpu = num(&s["cpu_stats"]["cpu_usage"]["total_usage"]) - num(&s["precpu_stats"]["cpu_usage"]["total_usage"]);
    let system = num(&s["cpu_stats"]["system_cpu_usage"]) - num(&s["precpu_stats"]["system_cpu_usage"]);
    let cpus = s["cpu_stats"]["online_cpus"].as_f64().filter(|n| *n > 0.0).unwrap_or(1.0);
    let percent = if system > 0.0 && cpu > 0.0 { cpu / system * cpus * 100.0 } else { 0.0 };
    // Without the page cache, as `docker stats`: cgroup v2, then v1
    let m = &s["memory_stats"];
    let cache = m["stats"]["inactive_file"].as_f64().or(m["stats"]["total_inactive_file"].as_f64()).unwrap_or(0.0);
    let used = (num(&m["usage"]) - cache).max(0.0);
    Some((percent, used / MB))
}

/// A size of `/system/df`, which says -1 for what it didn't measure.
fn bytes_of(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0).max(0.0)
}

/// What `docker system df` says, from `/system/df`: what each kind uses and what a clean-up of it
/// (as `Prune::targets`) frees, with the images, the volumes and the build cache it would remove;
/// and the networks of `/networks` that one could. `named_too`: an Engine before 23, whose
/// `volume prune` removes named volumes as well.
fn disk_usage(df: &Value, networks: &Value, named_too: bool) -> Disk {
    let items = |key: &str| df[key].as_array().cloned().unwrap_or_default();
    let split = |list: Vec<Value>, size: fn(&Value) -> f64, free: fn(&Value) -> bool| -> (f64, f64) {
        (list.iter().map(size).sum(), list.iter().filter(|i| free(i)).map(size).sum())
    };
    // As the CLI: the layers, less what the images that containers use don't share
    let layers = bytes_of(&df["LayersSize"]);
    let used: f64 = items("Images")
        .iter()
        .filter(|i| i["Containers"].as_i64().unwrap_or(0) > 0)
        .map(|i| (bytes_of(&i["Size"]) - bytes_of(&i["SharedSize"])).max(0.0))
        .sum();
    let image_list = items("Images")
        .iter()
        .map(|i| Image {
            id: i["Id"].as_str().unwrap_or_default().to_string(),
            tags: i["RepoTags"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|t| *t != "<none>:<none>").map(String::from).collect(),
            size: bytes_of(&i["Size"]),
            shared: bytes_of(&i["SharedSize"]),
            containers: i["Containers"].as_i64().unwrap_or(0),
        })
        .collect();
    let loose_volume = |v: &Value| v["UsageData"]["RefCount"].as_i64() == Some(0) && (named_too || v["Labels"].get("com.docker.volume.anonymous").is_some());
    let loose_build = |b: &Value| b["InUse"] != Value::Bool(true);
    let volumes = items("Volumes");
    let volume_size = |v: &Value| bytes_of(&v["UsageData"]["Size"]);
    // As `network prune`: local, not Docker's own, not a template for others
    let networks = networks
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| n["Scope"].as_str() == Some("local") && n["ConfigOnly"] != Value::Bool(true) && !matches!(n["Name"].as_str(), Some("bridge" | "host" | "none")))
        .map(|n| Network { id: n["Id"].as_str().unwrap_or_default().to_string(), name: n["Name"].as_str().unwrap_or_default().to_string() })
        .collect();
    Disk {
        images: (layers, (layers - used).max(0.0)),
        containers: split(items("Containers"), |c| bytes_of(&c["SizeRw"]), |c| !matches!(c["State"].as_str(), Some("running" | "paused" | "restarting"))),
        volumes: (volumes.iter().map(volume_size).sum(), volumes.iter().filter(|v| loose_volume(v)).map(volume_size).sum()),
        build: split(items("BuildCache"), |b| bytes_of(&b["Size"]), |b| b["InUse"] != Value::Bool(true)),
        image_list,
        layers,
        loose_volumes: volumes.iter().filter(|v| loose_volume(v)).count(),
        loose_build: items("BuildCache").iter().filter(|b| loose_build(b)).count(),
        networks,
    }
}

/// Measuring volumes and the layers of containers walks their files: it can take a while.
fn disk(path: &Path, named_too: bool) -> Option<Disk> {
    let df = get(path, "/system/df", Duration::from_secs(60))?;
    let networks = get(path, "/networks", Duration::from_secs(10))?;
    Some(disk_usage(&df, &networks, named_too))
}

/// Runs a clean-up: what it removed and freed, or why it failed.
fn prune(path: &Path, kind: Prune) -> Result<Report, String> {
    let mut report = Report::default();
    let count = |body: &Value, key: &str| body[key].as_array().map_or(0, Vec::len);
    for target in kind.targets() {
        let body = call(path, "POST", &target)?;
        report.freed += bytes_of(&body["SpaceReclaimed"]);
        report.containers += count(&body, "ContainersDeleted");
        report.volumes += count(&body, "VolumesDeleted");
        report.networks += count(&body, "NetworksDeleted");
    }
    Ok(report)
}

fn decimal(v: f64) -> String {
    let s = format!("{v:.1}");
    if crate::i18n::italian() { s.replace('.', ",") } else { s }
}

/// `850 MB`, `3,2 GB`, `640 KB`.
fn size(mb: f64) -> String {
    if mb >= 1024.0 {
        format!("{} GB", decimal(mb / 1024.0))
    } else if mb >= 1.0 || mb == 0.0 {
        // The sum of nothing (no build cache) is -0.0, which prints as -0
        format!("{:.0} MB", mb.abs())
    } else {
        format!("{:.0} KB", mb * 1024.0)
    }
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

/// Italian or English, from two texts made for it.
fn tt(it: impl Into<String>, en: impl Into<String>) -> String {
    if crate::i18n::italian() { it.into() } else { en.into() }
}

/// Exited with an error: 130, 137 and 143 are interrupted, killed or stopped by `docker stop`.
fn failed(c: &Container) -> Option<i64> {
    c.exit.filter(|code| ![0, 130, 137, 143].contains(code))
}

/// The state of a container in words.
fn state_text(c: &Container) -> String {
    let text = match (c.state.as_str(), c.health.as_str()) {
        ("paused", _) => t("In pausa", "Paused"),
        ("restarting", _) => t("In riavvio", "Restarting"),
        ("running", "unhealthy") => t("Health check fallito", "Health check failing"),
        ("running", "starting") => t("In avvio", "Starting"),
        ("running", _) => t("In esecuzione", "Running"),
        ("created", _) => t("Creato", "Created"),
        ("removing", _) => t("Eliminazione…", "Removing…"),
        ("dead", _) => t("Guasto", "Dead"),
        _ => match failed(c) {
            Some(code) => return tt(format!("Uscito con codice {code}"), format!("Exited with code {code}")),
            None => t("Fermo", "Stopped"),
        },
    };
    text.to_string()
}

/// The look of a container: its icon, the icon's color, and what the tile says instead of the
/// ports and the uptime, in the same color.
fn look(c: &Container, th: &Theme) -> (&'static str, Rgba, Option<(String, Rgba)>) {
    let (symbol, color, says) = match (c.state.as_str(), c.health.as_str()) {
        ("paused", _) => ("pause.circle.fill", th.yellow, true),
        ("restarting", _) => ("arrow.clockwise.circle.fill", th.red, true),
        ("running", "unhealthy") => ("exclamationmark.circle.fill", th.red, true),
        ("running", "starting") => ("shippingbox.fill", th.yellow, true),
        ("running", _) => ("shippingbox.fill", th.green, false),
        ("created", _) => ("shippingbox", th.subtext, true),
        ("removing", _) => ("trash", th.subtext, true),
        ("dead", _) => ("xmark.circle.fill", th.red, true),
        _ if failed(c).is_some() => ("exclamationmark.circle.fill", th.red, true),
        _ => ("shippingbox", th.subtext, true),
    };
    (symbol, color, says.then(|| (state_text(c), color)))
}

/// `a`, `a e b`, `a, b e c`.
fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} {} {last}", rest.join(", "), t("e", "and")),
    }
}

/// `~/Projects/shop`, for a folder in the home.
fn tilde(dir: &str) -> String {
    match Path::new(dir).strip_prefix(crate::config::home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => dir.to_string(),
    }
}

/// The images by their tags, as the popup shows them; the untagged ones counted at the end.
fn image_names(images: &[&Image]) -> Vec<String> {
    let mut names: Vec<String> = images.iter().flat_map(|i| i.tags.iter().map(|tag| short_image(tag))).collect();
    names.sort();
    names.dedup();
    let untagged = images.iter().filter(|i| i.tags.is_empty()).count() as u64;
    if untagged > 0 {
        names.push(count(untagged, ("senza nome", "untagged"), ("senza nome", "untagged")));
    }
    names
}

/// The images that `image prune -a` removes after `container prune`, as `system prune -a` does:
/// those that no running container uses. Only when every container that uses one is known, so
/// that one in use is never said to go.
fn loose_images<'a>(disk: &'a Disk, containers: &[Container]) -> Vec<&'a Image> {
    disk.image_list
        .iter()
        .filter(|i| {
            let users: Vec<&Container> = containers.iter().filter(|c| c.uses(i, &disk.image_list)).collect();
            i.containers == 0 || (users.len() as i64 >= i.containers && users.iter().all(|c| !c.up()))
        })
        .collect()
}

/// The networks that `network prune` removes: those that no container up is on (Docker keeps a
/// network while it has endpoints, which stopped containers don't have).
fn loose_networks<'a>(disk: &'a Disk, containers: &[Container]) -> Vec<&'a Network> {
    disk.networks.iter().filter(|n| !containers.iter().any(|c| c.up() && c.networks.iter().any(|on| *on == n.id || *on == n.name))).collect()
}

/// How to get removed containers back: `docker compose up` makes those of a project again (and
/// pulls or builds their images if they're gone), the others need their `docker run`. Each line
/// names what it brings back, unless there's only one.
fn back(gone: &[&Container]) -> Vec<String> {
    let mut projects: Vec<(&str, &str)> = gone.iter().filter(|c| !c.project.is_empty()).map(|c| (c.project.as_str(), c.dir.as_str())).collect();
    projects.sort();
    projects.dedup();
    let others: Vec<&Container> = gone.iter().copied().filter(|c| c.project.is_empty()).collect();
    let single = usize::from(!others.is_empty()) + projects.len() == 1;
    // `Per riaverlo`, or `Per riavere shop-worker-1`
    let who = |list: &[&Container]| -> (String, String) {
        let names: Vec<String> = list.iter().map(|c| c.name.clone()).collect();
        match (single, list.len()) {
            (true, 1) => ("Per riaverlo".into(), "To get it back".into()),
            (true, _) => ("Per riaverli".into(), "To get them back".into()),
            (false, n) if n > 3 => (format!("Per riavere i {n} container"), format!("To get the {n} containers back")),
            _ => (format!("Per riavere {}", and_list(&names)), format!("To get {} back", and_list(&names))),
        }
    };
    let mut lines = Vec::new();
    if projects.len() > 3 {
        let names: Vec<String> = projects.iter().map(|(p, _)| p.to_string()).collect();
        lines.push(tt(
            format!("Per riavere i container dei progetti: docker compose up -d nella cartella di ognuno ({})", and_list(&names)),
            format!("To get the projects' containers back: docker compose up -d in the folder of each ({})", and_list(&names)),
        ));
    } else {
        for (project, dir) in &projects {
            let theirs: Vec<&Container> = gone.iter().copied().filter(|c| c.project == *project).collect();
            let (it, en) = who(&theirs);
            lines.push(if dir.is_empty() {
                tt(format!("{it}: docker compose up -d nella cartella di {project}"), format!("{en}: docker compose up -d in the folder of {project}"))
            } else {
                tt(format!("{it}: docker compose up -d in {}", tilde(dir)), format!("{en}: docker compose up -d in {}", tilde(dir)))
            });
        }
    }
    if !others.is_empty() {
        let (it, en) = who(&others);
        let (it_verb, en_verb) = if others.len() == 1 { ("va ricreato", "create it again") } else { ("vanno ricreati", "create them again") };
        lines.push(tt(
            format!("{it} {it_verb} con docker run e le stesse opzioni (porte, volumi, variabili)."),
            format!("{en}, {en_verb} with docker run and the same options (ports, volumes, variables)."),
        ));
    }
    lines
}

/// What a clean-up did, for its note.
fn done_text(kind: Prune, r: Report) -> String {
    let freed = || if r.freed > 0.0 { format!(" · {}", size(r.freed / MB)) } else { String::new() };
    match kind {
        Prune::Networks if r.networks == 0 => t("Niente da eliminare", "Nothing to remove").into(),
        Prune::Networks => count(r.networks as u64, ("rete eliminata", "network removed"), ("reti eliminate", "networks removed")),
        Prune::Containers if r.containers == 0 => t("Niente da eliminare", "Nothing to remove").into(),
        Prune::Containers => count(r.containers as u64, ("eliminato", "removed"), ("eliminati", "removed")) + &freed(),
        Prune::Volumes if r.volumes == 0 => t("Niente da eliminare", "Nothing to remove").into(),
        Prune::Volumes => count(r.volumes as u64, ("eliminato", "removed"), ("eliminati", "removed")) + &freed(),
        _ if r.freed == 0.0 => t("Niente da liberare", "Nothing to free").into(),
        _ => t(&format!("Liberati {}", size(r.freed / MB)), &format!("{} freed", size(r.freed / MB))).to_string(),
    }
}

impl Docker {
    pub fn new(_options: &Options) -> Docker {
        let mock = crate::mock::load::<Mock>("docker");
        let mut d = Docker {
            snapshot: None,
            usage: HashMap::new(),
            disk: None,
            socket: None,
            checked: false,
            events: None,
            burst: 0,
            measuring: false,
            sizing: false,
            again: false,
            notes: HashMap::new(),
            asking: None,
            mock: mock.is_some(),
        };
        if let Some(m) = mock {
            let now = crate::time::now();
            let mut containers = m.containers;
            for c in &mut containers {
                c.started = c.started.map(|ago| now - ago);
                // The notes are by id
                if c.id.is_empty() {
                    c.id = c.name.clone();
                }
            }
            sort(&mut containers);
            d.snapshot = m.engine.map(|engine| Snapshot { engine, containers });
            d.usage = m.usage;
            d.disk = m.disk;
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
                let filters = query(r#"{"type":["container"],"event":["create","start","die","pause","unpause","health_status","rename","destroy"]}"#);
                let Ok(response) = net::unix_get(&path, &format!("/events?since={since}&filters={filters}"), None) else {
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
        match &snapshot {
            // The notes of containers that are gone go with them
            Some(s) => {
                self.notes.retain(|key, _| key.starts_with("prune:") || s.containers.iter().any(|c| c.id == *key));
                if let Some((Ask::Remove(id), _)) = &self.asking
                    && !s.containers.iter().any(|c| c.id == *id)
                {
                    self.asking = None;
                }
            }
            None => {
                self.usage.clear();
                self.notes.clear();
                self.asking = None;
                if cx.popup_open {
                    cx.close_popup();
                }
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

    /// CPU and memory of the running containers, while the popup is open.
    fn measure(&mut self, cx: &mut Cx) {
        if self.mock || self.measuring || !cx.popup_open || self.asking.is_some() {
            return;
        }
        let (Some(path), Some(snap)) = (self.socket.clone(), &self.snapshot) else { return };
        let shown: Vec<(String, String)> = snap.containers.iter().filter(|c| c.state == "running").take(MAX).map(|c| (c.id.clone(), c.name.clone())).collect();
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

    /// The disk, when the popup opens and after an action.
    fn size_up(&mut self, cx: &mut Cx) {
        if self.mock {
            return;
        }
        if self.sizing {
            self.again = true;
            return;
        }
        let Some(path) = self.socket.clone() else { return };
        self.sizing = true;
        let named_too = self.snapshot.as_ref().and_then(|s| s.engine.version.split('.').next()?.parse::<u32>().ok()).is_some_and(|major| major < 23);
        cx.background::<Docker, Option<Disk>>(move || disk(&path, named_too), |w, disk, cx| {
            w.sizing = false;
            if disk.is_some() && w.disk != disk {
                w.disk = disk;
            }
            // A question waits for it, even when nothing changed
            cx.redraw();
            if std::mem::take(&mut w.again) {
                w.size_up(cx);
            }
        });
    }

    /// A note that goes by itself after `secs`, unless replaced.
    fn note_for(&mut self, key: &str, note: Note, secs: f64, cx: &mut Cx) {
        self.notes.insert(key.to_string(), note.clone());
        let key = key.to_string();
        cx.after::<Docker>(secs, move |w, cx| {
            if w.notes.get(&key) == Some(&note) {
                w.notes.remove(&key);
                cx.redraw();
            }
        });
        cx.redraw();
    }

    /// A button of a container (removing, once the popup has asked).
    fn act(&mut self, id: &str, op: Op, cx: &mut Cx) {
        let Some(c) = self.snapshot.as_ref().and_then(|s| s.containers.iter().find(|c| c.id == id)) else { return };
        if !c.can(op) || matches!(self.notes.get(id), Some(Note::Busy(_))) {
            return;
        }
        let (method, target, busy) = match op {
            Op::Start if c.state == "paused" => ("POST", format!("/containers/{id}/unpause"), t("Riprende…", "Resuming…")),
            Op::Start => ("POST", format!("/containers/{id}/start"), t("Si avvia…", "Starting…")),
            Op::Stop => ("POST", format!("/containers/{id}/stop"), t("Si ferma…", "Stopping…")),
            Op::Restart => ("POST", format!("/containers/{id}/restart"), t("Si riavvia…", "Restarting…")),
            // Running too, as `docker rm -f`; its anonymous volumes stay, for a clean-up
            Op::Remove => ("DELETE", format!("/containers/{id}?force=true"), t("Eliminazione…", "Removing…")),
        };
        // Made-up data: nothing happens
        if self.mock {
            self.notes.remove(id);
            return cx.redraw();
        }
        let Some(path) = self.socket.clone() else { return };
        let (key, name) = (id.to_string(), c.name.clone());
        self.notes.insert(key.clone(), Note::Busy(busy));
        cx.background::<Docker, Result<Value, String>>(
            move || call(&path, method, &target),
            move |w, result, cx| w.finish(key, result.map(|_| None).map_err(|why| format!("{name}: {why}")), cx),
        );
        cx.redraw();
    }

    /// ▶ ■ ↻ of a Compose project: the same on each of its containers that it does something to.
    fn act_project(&mut self, project: &str, op: Op, cx: &mut Cx) {
        let ids: Vec<String> = self.snapshot.iter().flat_map(|s| &s.containers).filter(|c| c.project == project && c.can(op)).map(|c| c.id.clone()).collect();
        for id in ids {
            self.act(&id, op, cx);
        }
    }

    /// A clean-up, once the popup has asked.
    fn clean(&mut self, kind: Prune, cx: &mut Cx) {
        let key = format!("prune:{}", kind.key());
        if matches!(self.notes.get(&key), Some(Note::Busy(_))) {
            return;
        }
        if self.mock {
            self.notes.remove(&key);
            return cx.redraw();
        }
        let Some(path) = self.socket.clone() else { return };
        self.notes.insert(key.clone(), Note::Busy(t("Pulizia…", "Cleaning up…")));
        cx.background::<Docker, Result<Report, String>>(
            move || prune(&path, kind),
            move |w, result, cx| {
                let result = result.map(|report| Some(Note::Done(done_text(kind, report))));
                w.finish(key, result.map_err(|why| format!("prune {}: {why}", kind.key())), cx);
            },
        );
        cx.redraw();
    }

    /// An action is done: `Ok` with what to say for a while, if anything. The containers are read
    /// again at once rather than after the events, and the busy note stays until then, so that the
    /// tile doesn't show the old state in between.
    fn finish(&mut self, key: String, result: Result<Option<Note>, String>, cx: &mut Cx) {
        match result {
            Err(why) => {
                crate::log(&format!("docker: {why}"));
                self.note_for(&key, Note::Failed, 5.0, cx);
            }
            Ok(note) => {
                if let Some(note) = note {
                    self.note_for(&key, note, 5.0, cx);
                }
                if let Some(path) = self.socket.clone() {
                    cx.background::<Docker, Option<Snapshot>>(move || snapshot(&path), move |w, snap, cx| {
                        if matches!(w.notes.get(&key), Some(Note::Busy(_))) {
                            w.notes.remove(&key);
                            cx.redraw();
                        }
                        let socket = snap.as_ref().and(w.socket.clone());
                        w.update(socket, snap, cx);
                    });
                }
                if cx.popup_open {
                    self.size_up(cx);
                }
            }
        }
    }

    /// A click that removes something: the popup asks first, saying what. A clean-up measures
    /// again what it would remove, so that the question is about how things are now.
    fn ask(&mut self, ask: Ask, cx: &mut Cx) {
        if let Ask::Remove(id) = &ask
            && !self.snapshot.iter().flat_map(|s| &s.containers).any(|c| c.id == *id && c.can(Op::Remove))
        {
            return;
        }
        let prune = matches!(ask, Ask::Prune(_));
        self.asking = Some((ask, crate::time::now()));
        if prune {
            self.size_up(cx);
        }
        cx.redraw();
    }

    /// The answer: `yes` does what the popup asked about, then the dashboard is back either way.
    fn answer(&mut self, yes: bool, cx: &mut Cx) {
        let Some((ask, at)) = self.asking.clone() else { return };
        if yes {
            // A double click on what asked isn't an answer: the button may be where it was
            if crate::time::now() - at < 0.5 {
                return;
            }
            if !self.sheet(&ask).is_some_and(|s| s.button.is_some() && !s.measuring) {
                return;
            }
        }
        self.asking = None;
        if yes {
            match ask {
                Ask::Remove(id) => self.act(&id, Op::Remove, cx),
                Ask::Prune(kind) => self.clean(kind, cx),
            }
        }
        self.measure(cx);
        cx.redraw();
    }

    /// What the popup says before removing something: what goes, by name, what stays, what's
    /// lost and how to get it back. None if what it was about is gone.
    fn sheet(&self, ask: &Ask) -> Option<Sheet> {
        let all = &self.snapshot.as_ref()?.containers;
        let lost = |text: String| ("trash.circle.fill", Hue::Lost, text);
        let keeps = |text: String| ("checkmark.circle.fill", Hue::Keeps, text);
        let warning = |text: String| ("exclamationmark.triangle.fill", Hue::Warning, text);
        let backs = |gone: &[&Container]| back(gone).into_iter().map(|text| ("arrow.uturn.backward.circle.fill", Hue::Back, text)).collect::<Vec<_>>();
        let kind = match ask {
            Ask::Remove(id) => {
                let c = all.iter().find(|c| c.id == *id)?;
                let mut notes = Vec::new();
                match c.state.as_str() {
                    "paused" => notes.push(("stop.circle.fill", Hue::Warning, tt("È in pausa: prima viene fermato.", "It's paused: it's stopped first."))),
                    _ if c.up() => notes.push((
                        "stop.circle.fill",
                        Hue::Warning,
                        tt("È in esecuzione: prima viene fermato, e si interrompe quello che sta facendo.", "It's running: it's stopped first, and whatever it's doing is interrupted."),
                    )),
                    _ => {}
                }
                notes.push(lost(tt("Vanno persi i file che ha scritto fuori dai volumi, e i suoi log.", "The files it wrote outside its volumes are lost, and its logs.")));
                notes.push(keeps(tt(format!("Restano l'immagine {} e i volumi, con i loro dati.", c.image), format!("The {} image and the volumes stay, with their data.", c.image))));
                notes.extend(backs(&[c]));
                return Some(Sheet {
                    symbol: "trash.fill",
                    title: tt(format!("Eliminare {}?", c.name), format!("Remove {}?", c.name)),
                    subtitle: format!("{} · {}", state_text(c), c.image),
                    goes: Vec::new(),
                    notes,
                    button: c.can(Op::Remove).then(|| tt("Elimina", "Remove")),
                    measuring: false,
                });
            }
            Ask::Prune(kind) => *kind,
        };
        let empty = Disk::default();
        let disk = self.disk.as_ref().unwrap_or(&empty);
        // The containers' clean-up knows them already; the others wait for the disk
        let measuring = kind != Prune::Containers && (self.disk.is_none() || self.sizing || self.again);
        let stopped: Vec<&Container> = all.iter().filter(|c| c.stopped()).collect();
        let up = all.iter().filter(|c| c.up()).count();
        let cli = |command: &str| tt(format!("Come {command}"), format!("As {command}"));
        let mut sheet = Sheet { measuring, ..Default::default() };
        let containers_goes = || Goes {
            symbol: "shippingbox.fill",
            what: count(stopped.len() as u64, ("container fermo", "stopped container"), ("container fermi", "stopped containers")),
            names: stopped.iter().map(|c| c.name.clone()).collect(),
            size: Some(disk.containers.1).filter(|s| *s > 0.0),
        };
        let networks = loose_networks(disk, all);
        let networks_goes = || Goes {
            symbol: "network",
            what: count(networks.len() as u64, ("rete senza container attivi", "network without running containers"), ("reti senza container attivi", "networks without running containers")),
            names: networks.iter().map(|n| n.name.clone()).collect(),
            size: None,
        };
        let build_goes = || Goes {
            symbol: "hammer.fill",
            what: tt("Cache di build non in uso", "Unused build cache"),
            names: vec![count(disk.loose_build as u64, ("elemento", "record"), ("elementi", "records"))],
            size: Some(disk.build.1),
        };
        let lost_files = || lost(tt("Vanno persi i file che i container fermi hanno scritto fuori dai volumi, e i loro log.", "The files the stopped containers wrote outside their volumes are lost, and their logs."));
        match kind {
            Prune::Containers => {
                sheet.symbol = "shippingbox.fill";
                sheet.subtitle = cli("docker container prune");
                if stopped.is_empty() {
                    sheet.title = tt("Nessun container fermo", "No stopped containers");
                    sheet.notes.push(keeps(tt("Non c'è niente da eliminare: i container sono tutti attivi.", "There's nothing to remove: the containers all run.")));
                    return Some(sheet);
                }
                let n = stopped.len();
                sheet.title = if n == 1 { tt("Eliminare 1 container fermo?", "Remove 1 stopped container?") } else { tt(format!("Eliminare {n} container fermi?"), format!("Remove {n} stopped containers?")) };
                sheet.goes.push(containers_goes());
                sheet.notes.push(lost_files());
                sheet.notes.push(keeps(if up > 0 {
                    tt(format!("Restano i container attivi e in pausa ({up}), le immagini e i volumi."), format!("Running and paused containers ({up}), images and volumes stay."))
                } else {
                    tt("Restano le immagini e i volumi.", "Images and volumes stay.")
                }));
                sheet.notes.push(warning(tt(
                    "Senza container le loro immagini diventano inutilizzate: una pulizia delle immagini, dopo, le eliminerebbe.",
                    "Without containers their images become unused: a clean-up of images would then remove them.",
                )));
                sheet.notes.extend(backs(&stopped));
                sheet.button = Some(if n == 1 { tt("Elimina 1 container", "Remove 1 Container") } else { tt(format!("Elimina {n} container"), format!("Remove {n} Containers")) });
            }
            Prune::Images => {
                sheet.symbol = "square.stack.3d.up.fill";
                sheet.subtitle = cli("docker image prune -a");
                let loose: Vec<&Image> = disk.image_list.iter().filter(|i| i.containers == 0).collect();
                let n = loose.len();
                sheet.title = tt("Eliminare le immagini inutilizzate?", "Remove the unused images?");
                if !measuring && n == 0 {
                    sheet.title = tt("Nessuna immagine inutilizzata", "No unused images");
                    sheet.notes.push(keeps(tt("Ogni immagine è usata da un container, anche fermo.", "Every image is used by a container, even a stopped one.")));
                    return Some(sheet);
                }
                sheet.goes.push(Goes {
                    symbol: "square.stack.3d.up.fill",
                    what: count(n as u64, ("immagine senza container", "image without containers"), ("immagini senza container", "images without containers")),
                    names: image_names(&loose),
                    size: Some(disk.images.1),
                });
                sheet.notes.push(keeps(tt("Restano le immagini usate da un container, anche fermo.", "Images used by a container, even a stopped one, stay.")));
                sheet.notes.push(("arrow.uturn.backward.circle.fill", Hue::Back, tt(
                    "Per riaverle si riscaricano (docker pull) o si ricostruiscono (docker build), e può volerci un po'.",
                    "To get them back, pull them again (docker pull) or build them (docker build), which can take a while.",
                )));
                sheet.button = Some(if n == 1 { tt("Elimina 1 immagine", "Remove 1 Image") } else { tt(format!("Elimina {n} immagini"), format!("Remove {n} Images")) });
            }
            Prune::Volumes => {
                sheet.symbol = "externaldrive.fill";
                sheet.subtitle = cli("docker volume prune");
                let version = self.snapshot.as_ref().map(|s| s.engine.version.clone()).unwrap_or_default();
                let named_too = version.split('.').next().and_then(|m| m.parse::<u32>().ok()).is_some_and(|major| major < 23);
                let n = disk.loose_volumes;
                sheet.title = if named_too { tt("Eliminare i volumi inutilizzati?", "Remove the unused volumes?") } else { tt("Eliminare i volumi anonimi inutilizzati?", "Remove the unused anonymous volumes?") };
                if !measuring && n == 0 {
                    sheet.title = tt("Nessun volume da eliminare", "No volumes to remove");
                    sheet.notes.push(keeps(tt("Ogni volume anonimo è di un container, anche fermo.", "Every anonymous volume belongs to a container, even a stopped one.")));
                    return Some(sheet);
                }
                let (one, many) = if named_too {
                    (("volume senza container", "volume without containers"), ("volumi senza container", "volumes without containers"))
                } else {
                    (("volume anonimo senza container", "anonymous volume without containers"), ("volumi anonimi senza container", "anonymous volumes without containers"))
                };
                sheet.goes.push(Goes {
                    symbol: "externaldrive.fill",
                    what: count(n as u64, one, many),
                    names: vec![tt(
                        "Senza nome: un container li crea da sé, e li lascia quando viene eliminato",
                        "Unnamed: a container makes them by itself, and leaves them behind when it's removed",
                    )],
                    size: Some(disk.volumes.1),
                });
                sheet.notes.push(lost(tt("I dati dentro vanno persi, e non si possono recuperare.", "The data in them is lost, and can't be recovered.")));
                sheet.notes.push(if named_too {
                    warning(tt(
                        format!("Con l'Engine {version} vanno via anche i volumi con un nome, se nessun container li usa."),
                        format!("With Engine {version} named volumes go too, if no container uses them."),
                    ))
                } else {
                    keeps(tt(
                        "Restano i volumi con un nome, come quelli di Compose, e quelli di un container, anche fermo.",
                        "Named volumes, as Compose's, and those of a container, even a stopped one, stay.",
                    ))
                });
                sheet.button = Some(if n == 1 { tt("Elimina 1 volume", "Remove 1 Volume") } else { tt(format!("Elimina {n} volumi"), format!("Remove {n} Volumes")) });
            }
            Prune::Build => {
                sheet.symbol = "hammer.fill";
                sheet.subtitle = cli("docker builder prune -a");
                sheet.title = tt("Svuotare la cache di build?", "Clear the build cache?");
                if !measuring && disk.loose_build == 0 && disk.build.1 == 0.0 {
                    sheet.title = tt("Niente da svuotare", "Nothing to clear");
                    sheet.notes.push(keeps(tt("La cache di build è vuota, o tutta in uso.", "The build cache is empty, or all in use.")));
                    return Some(sheet);
                }
                sheet.goes.push(build_goes());
                sheet.notes.push(keeps(tt("Restano i container, le immagini e i volumi.", "Containers, images and volumes stay.")));
                sheet.notes.push(("clock.fill", Hue::Info, tt(
                    "La prossima build riparte da zero, più lenta; poi la cache si rifà da sola.",
                    "The next build starts from scratch, slower; then the cache fills up again by itself.",
                )));
                sheet.button = Some(tt("Svuota la cache", "Clear Cache"));
            }
            Prune::Networks => {
                sheet.symbol = "network";
                sheet.subtitle = cli("docker network prune");
                let n = networks.len();
                sheet.title = tt("Eliminare le reti inutilizzate?", "Remove the unused networks?");
                if !measuring && n == 0 {
                    sheet.title = tt("Nessuna rete inutilizzata", "No unused networks");
                    sheet.notes.push(keeps(tt("Ogni rete è di Docker o di un container attivo.", "Every network is Docker's own or a running container's.")));
                    return Some(sheet);
                }
                sheet.goes.push(networks_goes());
                sheet.notes.push(keeps(tt(
                    "Restano le reti di Docker (bridge, host, none) e quelle dei container attivi.",
                    "Docker's own networks (bridge, host, none) and those of running containers stay.",
                )));
                // Docker removes a network that only stopped containers are on: they don't start again
                let stranded: Vec<&Container> = stopped.iter().copied().filter(|c| c.networks.iter().any(|on| networks.iter().any(|n| *on == n.id || *on == n.name))).collect();
                if !stranded.is_empty() {
                    let names = and_list(&stranded.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
                    let compose = if stranded.iter().any(|c| !c.project.is_empty()) { " (docker compose up -d --force-recreate)" } else { "" };
                    sheet.notes.push(warning(if stranded.len() == 1 {
                        tt(
                            format!("{names} è fermo su una di queste reti: senza, non riparte con ▶ e va ricreato{compose}."),
                            format!("{names} is stopped on one of them: without it, it won't start again with ▶ and has to be created again{compose}."),
                        )
                    } else {
                        tt(
                            format!("{names} sono fermi su queste reti: senza, non ripartono con ▶ e vanno ricreati{compose}."),
                            format!("{names} are stopped on them: without them, they won't start again with ▶ and have to be created again{compose}."),
                        )
                    }));
                }
                sheet.notes.push(("arrow.uturn.backward.circle.fill", Hue::Back, tt(
                    "Per riaverle: docker compose up -d rifà quelle di un progetto, docker network create le altre.",
                    "To get them back: docker compose up -d makes a project's again, docker network create the others.",
                )));
                sheet.button = Some(if n == 1 { tt("Elimina 1 rete", "Remove 1 Network") } else { tt(format!("Elimina {n} reti"), format!("Remove {n} Networks")) });
            }
            Prune::All => {
                sheet.symbol = "sparkles";
                sheet.subtitle = cli("docker system prune -a");
                sheet.title = tt("Pulire tutto?", "Clean up everything?");
                let images = loose_images(disk, all);
                if !stopped.is_empty() {
                    sheet.goes.push(containers_goes());
                }
                if !networks.is_empty() {
                    sheet.goes.push(networks_goes());
                }
                if !images.is_empty() {
                    // As the CLI: the layers, less what the images that stay don't share
                    let kept: f64 = disk.image_list.iter().filter(|i| !images.iter().any(|g| g.id == i.id)).map(Image::own).sum();
                    sheet.goes.push(Goes {
                        symbol: "square.stack.3d.up.fill",
                        what: count(images.len() as u64, ("immagine senza container attivi", "image without running containers"), ("immagini senza container attivi", "images without running containers")),
                        names: image_names(&images),
                        size: Some((disk.layers - kept).max(0.0)),
                    });
                }
                if disk.loose_build > 0 || disk.build.1 > 0.0 {
                    sheet.goes.push(build_goes());
                }
                if !measuring && sheet.goes.is_empty() {
                    sheet.title = tt("Niente da pulire", "Nothing to clean up");
                    sheet.notes.push(keeps(tt("Container, reti e immagini sono tutti in uso, e la cache di build è vuota.", "Containers, networks and images are all in use, and the build cache is empty.")));
                    return Some(sheet);
                }
                sheet.notes.push(keeps(tt(
                    "Restano i container attivi e in pausa, con le loro immagini e reti, e tutti i volumi con i loro dati.",
                    "Running and paused containers stay, with their images and networks, and all volumes with their data.",
                )));
                if !stopped.is_empty() {
                    sheet.notes.push(lost_files());
                }
                if stopped.iter().any(|c| images.iter().any(|i| c.uses(i, &disk.image_list))) {
                    sheet.notes.push(warning(tt(
                        "Con i container fermi vanno via anche le loro immagini: per riaverli vanno riscaricate o ricostruite, e può volerci un po'.",
                        "The stopped containers' images go with them: getting them back means pulling or building those again, which can take a while.",
                    )));
                }
                sheet.notes.extend(backs(&stopped));
                sheet.button = Some(tt("Elimina tutto", "Remove All"));
            }
        }
        Some(sheet)
    }

    // Drawing

    /// The engine, two tiles wide, with what all the containers use.
    fn engine(&self, rx: &Rx, snap: &Snapshot, title: &str) -> Node {
        let th = rx.theme;
        let e = &snap.engine;
        let blue = Rgba::parse(BLUE).unwrap_or(th.primary);
        let icon = if Path::new(DESKTOP).exists() { Node::image(WHALE, 0.0, 16.0, Some(blue)) } else { Node::symbol("shippingbox.fill", 18.0, Weight::Semibold, blue) };
        let detail = format!("Engine {} · {} · {}", e.version, count(e.cpus, ("CPU", "CPU"), ("CPU", "CPUs")), size(e.memory / MB));
        let mut row = vec![
            Node::row(vec![icon]).width(26.0).justify(Justify::Center).key("icon"),
            Node::column(vec![Node::text(title, th.font(), th.text).key("name"), Node::text(detail, th.small_font(), th.subtext).key("detail")]).gap(2.0).key("names"),
            Node::spacer(),
        ];
        let used: Vec<&(f64, f64)> = snap.containers.iter().filter(|c| c.state == "running").filter_map(|c| self.usage.get(&c.name)).collect();
        if !used.is_empty() {
            let (cpu, mb) = used.iter().fold((0.0, 0.0), |(cpu, mb), (c, m)| (cpu + c, mb + m));
            row.push(
                Node::column(vec![Node::text(format!("{}%", decimal(cpu)), th.font_with(Weight::Medium, 15.0), th.text).key("cpu"), Node::text(size(mb), th.small_font(), th.subtext).key("memory")])
                    .gap(2.0)
                    .align(Align::End)
                    .key("usage"),
            );
        }
        let up = snap.containers.iter().any(Container::up);
        Node::row(row).gap(10.0).padding(10.0, 10.0, 10.0, 10.0).width(WIDE).radius(10.0).background(th.tile(blue, up)).key("engine")
    }

    /// The title of a project, or of the containers without one: how many run, and ▶ ■ ↻ for a
    /// project.
    fn title(&self, rx: &Rx, group: &[Container], first: bool) -> Node {
        let th = rx.theme;
        let project = &group[0].project;
        let text = if project.is_empty() { t("Container", "Containers") } else { project.as_str() };
        let running = group.iter().filter(|c| c.up()).count() as u64;
        let mut row = vec![
            Node::text(text, th.font(), th.text).max_width(WIDE - 150.0).key("name"),
            Node::text(count(running, ("attivo", "running"), ("attivi", "running")), th.small_font(), th.subtext).key("count"),
            Node::spacer(),
        ];
        if !project.is_empty() {
            let prefix = format!("{project}-");
            for (symbol, op, name) in [("play.fill", Op::Start, "start"), ("stop.fill", Op::Stop, "stop"), ("arrow.clockwise", Op::Restart, "restart")] {
                let targets: Vec<String> = group.iter().filter(|c| c.can(op)).map(|c| c.name.strip_prefix(&prefix).filter(|n| !n.is_empty()).unwrap_or(&c.name).to_string()).collect();
                let enabled = !targets.is_empty();
                let which = if targets.len() <= 3 { and_list(&targets) } else { count(targets.len() as u64, ("container", "container"), ("container", "containers")) };
                let tip = match op {
                    Op::Start => tt(format!("Avvia {which} di {project}"), format!("Start {which} in {project}")),
                    Op::Stop => tt(format!("Ferma {which} di {project}"), format!("Stop {which} in {project}")),
                    _ => tt(format!("Riavvia {which} di {project}"), format!("Restart {which} in {project}")),
                };
                let mut button = Node::row(vec![Node::symbol(symbol, 10.0, Weight::Bold, if enabled { th.text } else { th.overlay })])
                    .size(28.0, 22.0)
                    .justify(Justify::Center)
                    .radius(6.0)
                    .background(th.surface.faded(if enabled { 0.7 } else { 0.35 }))
                    .key(name);
                if enabled {
                    button = button.click(rx.action(&format!("project:{name}:{project}"))).tip(tip);
                }
                row.push(button);
            }
        }
        Node::row(row).gap(6.0).padding(if first { 2.0 } else { 6.0 }, 0.0, 0.0, 10.0).key(format!("title-{project}"))
    }

    /// A container: its state, usage, name, image, ports and buttons.
    fn tile(&self, rx: &Rx, c: &Container, name: &str) -> Node {
        let th = rx.theme;
        let small = th.small_font();
        let (symbol, color, state) = look(c, th);
        let note = self.notes.get(&c.id);
        let busy = matches!(note, Some(Note::Busy(_)));
        let state = match note {
            Some(Note::Busy(text)) => Some((text.to_string(), th.yellow)),
            Some(Note::Failed) => Some((t("Non riuscito", "Failed").to_string(), th.red)),
            Some(Note::Done(text)) => Some((text.clone(), th.green)),
            None => state,
        };
        let status = match state {
            Some((text, color)) => vec![Node::text(text, small.clone(), color).max_width(INNER).key("state")],
            None => {
                // The ports open in the browser
                let mut line: Vec<Node> = c
                    .ports
                    .iter()
                    .take(2)
                    .map(|p| {
                        Node::row(vec![Node::text(format!(":{p}"), small.clone(), th.primary)])
                            .padding(0.0, 4.0, 0.0, 4.0)
                            .height(16.0)
                            .radius(4.0)
                            .background(th.primary.faded(0.14))
                            .click(rx.action(&format!("port:{p}")))
                            .tip(tt(format!("Apri http://localhost:{p}"), format!("Open http://localhost:{p}")))
                            .key(format!("port-{p}"))
                    })
                    .collect();
                if c.ports.len() > 2 {
                    let rest: Vec<String> = c.ports[2..].iter().map(|p| format!(":{p}")).collect();
                    line.push(Node::text(format!("+{}", rest.len()), small.clone(), th.subtext).tip(tt(format!("Anche {}", rest.join(", ")), format!("Also {}", rest.join(", ")))).key("more"));
                }
                let up = c.started.map(up_for).unwrap_or_else(|| t("In esecuzione", "Running").to_string());
                line.push(Node::text(up, small.clone(), th.subtext).key("up"));
                line
            }
        };
        let usage = self.usage.get(&c.name).filter(|_| c.state == "running").map(|(cpu, mb)| format!("{}% · {}", decimal(*cpu), size(*mb))).unwrap_or_default();
        let button = |symbol: &str, op: Op, action: &str, tip: String| {
            let enabled = c.can(op) && !busy;
            let color = if enabled { th.text } else { th.overlay };
            let background = th.surface.faded(if enabled { 0.7 } else { 0.35 });
            let mut b = Node::row(vec![Node::symbol(symbol, 11.0, Weight::Bold, color)]).size(BUTTON, 24.0).justify(Justify::Center).radius(6.0).background(background).key(action);
            if enabled {
                b = b.click(rx.action(&format!("{action}:{}", c.id))).tip(tip);
            }
            b
        };
        let start = if c.state == "paused" { tt(format!("Riprendi {name}"), format!("Resume {name}")) } else { tt(format!("Avvia {name}"), format!("Start {name}")) };
        Node::column(vec![
            Node::row(vec![Node::symbol(symbol, 18.0, Weight::Semibold, color).key("icon"), Node::spacer(), Node::text(usage, small.clone(), th.subtext).key("usage")]).height(22.0).key("top"),
            Node::text(name, th.font(), th.text).max_width(INNER).key("name"),
            Node::text(&c.image, small.clone(), th.subtext).max_width(INNER).key("image"),
            Node::row(status).gap(4.0).height(16.0).key("status"),
            Node::row(vec![
                button("play.fill", Op::Start, "start", start),
                button("stop.fill", Op::Stop, "stop", tt(format!("Ferma {name}"), format!("Stop {name}"))),
                button("arrow.clockwise", Op::Restart, "restart", tt(format!("Riavvia {name}"), format!("Restart {name}"))),
                button("trash", Op::Remove, "remove", tt(format!("Elimina {name}: prima mostra cosa succede"), format!("Remove {name}: shows what happens first"))),
            ])
                .justify(Justify::Between)
                .padding(6.0, 0.0, 0.0, 0.0)
                .key("buttons"),
        ])
        .gap(2.0)
        .padding(10.0, 10.0, 10.0, 10.0)
        .width(TILE)
        .radius(10.0)
        .background(th.tile(color, c.up()))
        .key(c.id.clone())
    }

    /// What a clean-up says over its state, if anything.
    fn prune_note(&self, rx: &Rx, key: &str) -> Option<(String, Rgba)> {
        let th = rx.theme;
        Some(match self.notes.get(key)? {
            Note::Busy(text) => (text.to_string(), th.yellow),
            Note::Failed => (t("Non riuscito", "Failed").to_string(), th.red),
            Note::Done(text) => (text.clone(), th.green),
        })
    }

    /// A kind of the disk: its size and what a clean-up would remove, which a click asks about.
    fn disk_tile(&self, rx: &Rx, kind: Prune) -> Node {
        let th = rx.theme;
        let disk = self.disk.as_ref();
        let stopped = self.snapshot.iter().flat_map(|s| &s.containers).filter(|c| c.stopped()).count() as u64;
        let (symbol, name, sizes, tip) = match kind {
            Prune::Images => (
                "square.stack.3d.up.fill",
                t("Immagini", "Images"),
                disk.map(|d| d.images),
                tt("Elimina le immagini che nessun container usa: prima mostra quali", "Removes the images no container uses: shows which first"),
            ),
            Prune::Containers => ("shippingbox.fill", t("Container", "Containers"), disk.map(|d| d.containers), tt("Elimina i container fermi: prima mostra quali", "Removes the stopped containers: shows which first")),
            Prune::Volumes => (
                "externaldrive.fill",
                t("Volumi", "Volumes"),
                disk.map(|d| d.volumes),
                tt("Elimina i volumi anonimi che nessun container usa: prima mostra quanti", "Removes the anonymous volumes no container uses: shows how many first"),
            ),
            _ => ("hammer.fill", t("Cache di build", "Build Cache"), disk.map(|d| d.build), tt("Svuota la cache di build: prima mostra quanto", "Clears the build cache: shows how much first")),
        };
        // Something for a clean-up to remove: the containers by how many are stopped, as their
        // files may take nothing
        let removable = match (kind, disk) {
            (Prune::Containers, _) => stopped > 0,
            (_, None) => false,
            (Prune::Images, Some(d)) => d.images.1 > 0.0 || d.image_list.iter().any(|i| i.containers == 0),
            (Prune::Volumes, Some(d)) => d.volumes.1 > 0.0 || d.loose_volumes > 0,
            (_, Some(d)) => d.build.1 > 0.0 || d.loose_build > 0,
        };
        let key = format!("prune:{}", kind.key());
        let (text, color) = self.prune_note(rx, &key).unwrap_or_else(|| match (kind, sizes) {
            (_, None) => (t("Calcolo…", "Measuring…").to_string(), th.subtext),
            (Prune::Containers, Some((_, free))) if stopped > 0 => {
                let n = count(stopped, ("fermo", "stopped"), ("fermi", "stopped"));
                (if free > 0.0 { format!("{n} · {}", size(free / MB)) } else { n }, th.text)
            }
            (Prune::Containers, _) => (t("Nessuno fermo", "None stopped").to_string(), th.subtext),
            (_, Some((_, free))) if removable => (tt(format!("{} liberabili", size(free / MB)), format!("{} reclaimable", size(free / MB))), th.text),
            _ => (t("Niente da liberare", "Nothing to free").to_string(), th.subtext),
        });
        let mut tile = Node::column(vec![
            Node::row(vec![
                Node::symbol(symbol, 18.0, Weight::Semibold, if removable { th.primary } else { th.subtext }).key("icon"),
                Node::spacer(),
                Node::text(sizes.map(|(total, _)| size(total / MB)).unwrap_or_default(), th.font_with(Weight::Medium, 15.0), th.text).key("size"),
            ])
            .height(22.0)
            .key("top"),
            Node::text(name, th.font(), th.text).key("name"),
            Node::text(text, th.small_font(), color).max_width(INNER).key("state"),
        ])
        .gap(2.0)
        .padding(10.0, 10.0, 10.0, 10.0)
        .width(TILE)
        .radius(10.0)
        .background(th.tile(th.primary, false))
        .key(key.clone());
        if sizes.is_some() && removable && !matches!(self.notes.get(&key), Some(Note::Busy(_))) {
            tile = tile.click(rx.action(&key)).tip(tip);
        }
        tile
    }

    /// The clean-up of the networks, or of everything: a click asks about it.
    fn prune_button(&self, rx: &Rx, kind: Prune) -> Node {
        let th = rx.theme;
        let key = format!("prune:{}", kind.key());
        let (symbol, label, tip) = match kind {
            Prune::Networks => (
                "network",
                t("Pulisci reti…", "Clean Up Networks…"),
                tt("Elimina le reti che nessun container attivo usa: prima mostra quali", "Removes the networks no running container uses: shows which first"),
            ),
            _ => (
                "sparkles",
                t("Pulisci tutto…", "Clean Up All…"),
                tt(
                    "Elimina container fermi, reti e immagini inutilizzate e la cache di build, non i volumi: prima mostra cosa",
                    "Removes stopped containers, unused networks and images and the build cache, not volumes: shows what first",
                ),
            ),
        };
        let busy = matches!(self.notes.get(&key), Some(Note::Busy(_)));
        let (symbol, color, label, text_color) = match self.prune_note(rx, &key) {
            Some((text, color)) => {
                let symbol = match self.notes.get(&key) {
                    Some(Note::Busy(_)) => "hourglass",
                    Some(Note::Done(_)) => "checkmark",
                    Some(Note::Failed) => "exclamationmark.circle.fill",
                    _ => symbol,
                };
                (symbol, color, text, if color == th.green { th.text } else { color })
            }
            None => (symbol, th.primary, label.to_string(), th.text),
        };
        let mut button = Node::row(vec![Node::symbol(symbol, 11.0, Weight::Bold, color).key("icon"), Node::text(label, th.font_with(Weight::Bold, 12.0), text_color).max_width(INNER - 10.0).key("label")])
            .gap(6.0)
            .justify(Justify::Center)
            .size(TILE, 30.0)
            .radius(15.0)
            .background(th.surface.faded(0.7))
            .key(key.clone());
        if !busy && self.disk.is_some() {
            button = button.click(rx.action(&key)).tip(tip);
        }
        button
    }

    /// The question, in place of the dashboard: what it's about, what goes, the notes, then
    /// Cancel and the button that does it, in red.
    fn sheet_view(&self, rx: &Rx, s: &Sheet) -> Node {
        let th = rx.theme;
        let small = th.small_font();
        let inner = WIDE - 20.0;
        let lines = |text: &str, font: &crate::gfx::FontSpec, width: f64, color: Rgba, key: &str| -> Vec<Node> {
            wrap(text, font, width).into_iter().enumerate().map(|(i, line)| Node::text(line, font.clone(), color).key(format!("{key}-{i}"))).collect()
        };

        let title_font = th.font_with(Weight::Bold, th.font_size + 1.0);
        let mut heading = lines(&s.title, &title_font, inner - 44.0, th.text, "title");
        heading.push(Node::text(&s.subtitle, small.clone(), th.subtext).max_width(inner - 44.0).key("subtitle"));
        let mut rows = vec![
            Node::row(vec![
                Node::row(vec![Node::symbol(s.symbol, 15.0, Weight::Semibold, th.red)]).size(34.0, 34.0).radius(17.0).justify(Justify::Center).background(th.red.faded(0.16)).key("icon"),
                Node::column(heading).gap(1.0).align(Align::Start).key("heading"),
            ])
            .gap(10.0)
            .key("head"),
        ];

        // What goes, by name
        let names_w = inner - 20.0 - 28.0 - 64.0;
        if s.measuring {
            rows.push(
                Node::row(vec![Node::symbol("hourglass", 12.0, Weight::Semibold, th.subtext), Node::text(t("Calcolo di cosa verrebbe eliminato…", "Working out what would be removed…"), small.clone(), th.subtext)])
                    .gap(8.0)
                    .padding(10.0, 10.0, 10.0, 10.0)
                    .width(inner)
                    .radius(10.0)
                    .background(th.tile(th.red, false))
                    .key("goes"),
            );
        } else if !s.goes.is_empty() {
            let items: Vec<Node> = s
                .goes
                .iter()
                .map(|g| {
                    let mut text = lines(&g.what, &th.font(), names_w, th.text, "what");
                    text.extend(names_in(&g.names, &small, names_w, 3).into_iter().enumerate().map(|(i, line)| Node::text(line, small.clone(), th.subtext).key(format!("names-{i}"))));
                    let mut row = vec![
                        Node::row(vec![Node::symbol(g.symbol, 14.0, Weight::Semibold, th.red)]).size(20.0, 16.0).justify(Justify::Center).key("icon"),
                        Node::column(text).gap(1.0).align(Align::Start).key("text"),
                        Node::spacer(),
                    ];
                    if let Some(bytes) = g.size {
                        row.push(Node::text(size(bytes / MB), th.font_with(Weight::Medium, th.font_size), th.text).key("size"));
                    }
                    Node::row(row).gap(8.0).align(Align::Start).width(inner - 20.0).key(format!("goes-{}", g.symbol))
                })
                .collect();
            rows.push(Node::column(items).gap(10.0).padding(10.0, 10.0, 10.0, 10.0).width(inner).radius(10.0).background(th.tile(th.red, true)).key("goes"));
        }

        // What stays, what's lost, how to get it back
        for (i, (symbol, hue, text)) in s.notes.iter().enumerate() {
            let color = match hue {
                Hue::Keeps => th.green,
                Hue::Lost => th.red,
                Hue::Warning => th.yellow,
                // Not the accent, which may be red
                Hue::Back => th.color("blue").unwrap_or(th.primary),
                Hue::Info => th.subtext,
            };
            rows.push(
                Node::row(vec![
                    Node::row(vec![Node::symbol(*symbol, 12.0, Weight::Semibold, color)]).size(16.0, 15.0).justify(Justify::Center).key("icon"),
                    Node::column(lines(text, &small, inner - 24.0, th.text, "line")).gap(1.0).align(Align::Start).key("text"),
                ])
                .gap(8.0)
                .align(Align::Start)
                .padding(0.0, 0.0, 0.0, 2.0)
                .key(format!("note-{i}")),
            );
        }

        // Cancel, and what it does: the button waits while it measures
        let label = |text: &str, color: Rgba| Node::text(text, th.font(), color).key("label");
        let half = if s.button.is_some() { (inner - 8.0) / 2.0 } else { inner };
        let mut buttons = vec![
            Node::row(vec![label(if s.button.is_some() { t("Annulla", "Cancel") } else { t("Chiudi", "Close") }, th.text)])
                .size(half, 30.0)
                .justify(Justify::Center)
                .radius(8.0)
                .background(th.surface.faded(0.7))
                .click(rx.action("cancel"))
                .key("cancel"),
        ];
        if let Some(text) = &s.button {
            let mut button = Node::row(vec![if s.measuring { label(t("Calcolo…", "Measuring…"), th.subtext) } else { label(text, th.base) }])
                .size(half, 30.0)
                .justify(Justify::Center)
                .radius(8.0)
                .background(if s.measuring { th.red.faded(0.25) } else { th.red })
                .key("confirm");
            if !s.measuring {
                button = button.click(rx.action("confirm")).hover(th.red.mix(th.text, 0.18));
            }
            buttons.push(button);
        }
        rows.push(Node::row(buttons).gap(8.0).width(inner).padding(4.0, 0.0, 0.0, 0.0).key("buttons"));

        Node::column(rows).gap(10.0).align(Align::Start).padding(10.0, 10.0, 6.0, 10.0).width(WIDE).key("sheet")
    }
}

/// `names` joined, in at most `lines` lines of `width`; those that don't fit are counted at the end.
fn names_in(names: &[String], font: &crate::gfx::FontSpec, width: f64, lines: usize) -> Vec<String> {
    let mut best = Vec::new();
    for shown in 1..=names.len() {
        let mut text = names[..shown].join(", ");
        let more = names.len() - shown;
        if more > 0 {
            text += &tt(format!(" e altri {more}"), format!(" and {more} more"));
        }
        let wrapped = wrap(&text, font, width);
        if wrapped.len() > lines && !best.is_empty() {
            break;
        }
        best = wrapped;
    }
    best
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
        let up: Vec<&Container> = snap.containers.iter().filter(|c| c.up()).collect();
        let failing = up.iter().any(|c| c.state == "restarting" || c.health == "unhealthy");
        let color = if failing { th.red } else if up.is_empty() { th.subtext } else { th.text };
        let icon = if Path::new(DESKTOP).exists() {
            Node::image(WHALE, 0.0, 12.0, Some(Rgba::parse(BLUE).unwrap_or(th.primary)))
        } else {
            Node::symbol("shippingbox.fill", th.icon_size, Weight::Semibold, th.text)
        };
        Some(rx.pill(vec![icon.key("icon"), Node::text(up.len().to_string(), th.font(), color).key("count")]).click(rx.toggle_popup()))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let snap = self.snapshot.as_ref()?;
        // A question in place of the dashboard
        if let Some((ask, _)) = &self.asking
            && let Some(sheet) = self.sheet(ask)
        {
            return Some(rx.popup(WIDE, vec![self.sheet_view(rx, &sheet)]).padding(8.0, 8.0, 8.0, 8.0));
        }
        let title = match snap.engine.os.as_str() {
            os @ ("Docker Desktop" | "OrbStack") => os,
            _ => "Docker",
        };
        let mut rows = vec![self.engine(rx, snap, title)];

        // The containers, by project, two in a row
        let mut list: Vec<Node> = Vec::new();
        for group in snap.containers.chunk_by(|a, b| a.project == b.project) {
            list.push(self.title(rx, group, list.is_empty()));
            let prefix = format!("{}-", group[0].project);
            for pair in group.chunks(2) {
                let tiles = pair.iter().map(|c| self.tile(rx, c, c.name.strip_prefix(&prefix).filter(|n| !n.is_empty()).unwrap_or(&c.name))).collect();
                list.push(Node::row(tiles).gap(GAP).align(Align::Stretch).key(format!("row-{}", pair[0].id)));
            }
        }
        if list.is_empty() {
            list.push(Node::row(vec![Node::text(t("Nessun container", "No containers"), th.font(), th.subtext)]).padding(6.0, 10.0, 6.0, 10.0).key("none"));
        }
        let height = (rx.display.frame.size.height - AROUND).max(TILE * 1.5);
        rows.push(Node::new(Kind::Scroll { max_height: height, start: None }).width(WIDE).key("containers").push(Node::column(list).gap(GAP)));

        // The disk and the clean-ups
        rows.push(Node::row(vec![Node::text(t("Spazio su disco", "Disk Usage"), th.font(), th.text)]).height(24.0).padding(6.0, 0.0, 0.0, 10.0).key("disk-title"));
        rows.push(Node::row(vec![self.disk_tile(rx, Prune::Images), self.disk_tile(rx, Prune::Containers)]).gap(GAP).align(Align::Stretch).key("disk-1"));
        rows.push(Node::row(vec![self.disk_tile(rx, Prune::Volumes), self.disk_tile(rx, Prune::Build)]).gap(GAP).align(Align::Stretch).key("disk-2"));
        rows.push(Node::row(vec![self.prune_button(rx, Prune::Networks), self.prune_button(rx, Prune::All)]).gap(GAP).key("prune"));

        if title == "Docker Desktop" && Path::new(DESKTOP).exists() {
            rows.push(rx.row_button(vec![Node::text(t("Apri Docker Desktop", "Open Docker Desktop"), th.font(), th.primary)], vec![], rx.action("open")).key("open"));
        }
        Some(rx.popup(WIDE, rows).gap(GAP).padding(8.0, 8.0, 6.0, 8.0))
    }

    fn ready(&self) -> bool {
        self.checked
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        let (kind, rest) = name.split_once(':').unwrap_or((name, ""));
        let op = |s: &str| match s {
            "start" => Some(Op::Start),
            "stop" => Some(Op::Stop),
            "restart" => Some(Op::Restart),
            _ => None,
        };
        match kind {
            "open" | "port" => {
                cx.close_popup();
                if self.mock {
                    return;
                }
                if kind == "open" {
                    runtime::open_app("com.docker.docker");
                } else {
                    runtime::open(&format!("http://localhost:{rest}"));
                }
            }
            "prune" => {
                if let Some(p) = Prune::ALL.into_iter().find(|p| p.key() == rest) {
                    self.ask(Ask::Prune(p), cx);
                }
            }
            "remove" => self.ask(Ask::Remove(rest.to_string()), cx),
            "confirm" | "cancel" => self.answer(kind == "confirm", cx),
            "project" => {
                if let Some((o, project)) = rest.split_once(':')
                    && let Some(o) = op(o)
                {
                    self.act_project(project, o, cx);
                }
            }
            _ => {
                if let Some(o) = op(kind) {
                    self.act(rest, o, cx);
                }
            }
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::PopupOpened => {
                self.asking = None;
                self.measure(cx);
                self.size_up(cx);
            }
            // A question goes with the popup: it's asked again from the start
            Event::PopupClosed => self.asking = None,
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
    use serde_json::json;

    #[test]
    fn images() {
        assert_eq!(short_image("ghcr.io/org/app:1.2@sha256:abc"), "app:1.2");
        assert_eq!(short_image("postgres:16"), "postgres:16");
        assert_eq!(short_image("sha256:0123456789abcdef0123"), "0123456789ab");
        assert_eq!(size(512.0), "512 MB");
        assert_eq!(size(0.5), "512 KB");
        assert_eq!(size(std::iter::empty::<f64>().sum()), "0 MB");
        assert!(size(3276.8).starts_with('3'));
    }

    #[test]
    fn states() {
        assert_eq!(health("Up 2 hours (healthy)"), "healthy");
        assert_eq!(health("Up 5 seconds (health: starting)"), "starting");
        assert_eq!(exit_code("Exited (137) 2 hours ago"), Some(137));
        assert_eq!(exit_code("Up 2 hours"), None);
        let c = |state: &str| Container { state: state.into(), ..Default::default() };
        assert!(c("exited").can(Op::Start) && !c("exited").can(Op::Stop));
        assert!(c("paused").can(Op::Start) && c("paused").can(Op::Stop));
        assert!(!c("running").can(Op::Start) && c("running").can(Op::Restart) && c("running").can(Op::Remove));
    }

    #[test]
    fn order() {
        let c = |name: &str, project: &str| Container { name: name.into(), project: project.into(), ..Default::default() };
        let mut list = vec![c("mailpit", ""), c("shop-web-1", "shop"), c("api-1", "api"), c("shop-db-1", "shop")];
        sort(&mut list);
        let names: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["api-1", "shop-db-1", "shop-web-1", "mailpit"]);
    }

    #[test]
    fn clean_ups() {
        assert_eq!(Prune::All.targets()[0], "/containers/prune");
        assert_eq!(Prune::All.targets().len(), 4);
        assert_eq!(Prune::Images.targets()[0], "/images/prune?filters=%7B%22dangling%22%3A%5B%22false%22%5D%7D");
        let df = json!({
            "LayersSize": 1000,
            "Images": [
                {"Id": "a", "RepoTags": ["app:1"], "Size": 600, "SharedSize": 100, "Containers": 1},
                {"Id": "b", "RepoTags": ["<none>:<none>"], "Size": 400, "SharedSize": 100, "Containers": 0}
            ],
            "Containers": [{"SizeRw": 10, "State": "running"}, {"SizeRw": 5, "State": "exited"}],
            "Volumes": [
                {"UsageData": {"Size": 50, "RefCount": 0}, "Labels": {"com.docker.volume.anonymous": ""}},
                {"UsageData": {"Size": 70, "RefCount": 0}, "Labels": null},
                {"UsageData": {"Size": -1, "RefCount": 1}}
            ],
            "BuildCache": [{"Size": 30, "InUse": false}, {"Size": 20, "InUse": true}]
        });
        let networks = json!([
            {"Id": "1", "Name": "bridge", "Scope": "local"},
            {"Id": "2", "Name": "shop_default", "Scope": "local"},
            {"Id": "3", "Name": "ingress", "Scope": "swarm"},
            {"Id": "4", "Name": "template", "Scope": "local", "ConfigOnly": true}
        ]);
        let d = disk_usage(&df, &networks, false);
        assert_eq!(d.images, (1000.0, 500.0));
        assert_eq!(d.containers, (15.0, 5.0));
        assert_eq!(d.volumes, (120.0, 50.0));
        assert_eq!(d.build, (50.0, 30.0));
        assert_eq!((d.loose_volumes, d.loose_build), (1, 1));
        assert_eq!(d.image_list[0].tags, ["app:1"]);
        assert!(d.image_list[1].tags.is_empty());
        let names: Vec<&str> = d.networks.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["shop_default"]);
        // Before Engine 23, named volumes go too
        assert_eq!(disk_usage(&df, &networks, true).loose_volumes, 2);
    }

    /// A popup on the made-up data, as `examples/mock/docker.json`.
    fn made_up() -> Docker {
        let mock: Mock = serde_json::from_str(include_str!("../../examples/mock/docker.json")).unwrap();
        let mut d = Docker::new(&Options::default());
        let mut containers = mock.containers;
        for c in &mut containers {
            c.id = c.name.clone();
        }
        sort(&mut containers);
        d.snapshot = mock.engine.map(|engine| Snapshot { engine, containers });
        d.disk = mock.disk;
        d.mock = true;
        d
    }

    fn goes(sheet: &Sheet) -> Vec<(&'static str, Vec<String>)> {
        sheet.goes.iter().map(|g| (g.symbol, g.names.clone())).collect()
    }

    #[test]
    fn questions() {
        let d = made_up();
        let images = |kind| d.sheet(&Ask::Prune(kind)).unwrap().goes.into_iter().find(|g| g.symbol == "square.stack.3d.up.fill").unwrap().names;

        // The stopped containers, by name, and how to get them back
        let s = d.sheet(&Ask::Prune(Prune::Containers)).unwrap();
        assert_eq!(goes(&s), [("shippingbox.fill", vec!["shop-worker-1".to_string(), "minio".to_string()])]);
        assert!(s.notes.iter().any(|(_, hue, text)| *hue == Hue::Back && text.contains("docker compose up -d") && text.contains("~/Projects/shop")));
        assert!(s.notes.iter().any(|(_, hue, text)| *hue == Hue::Back && text.contains("minio") && text.contains("docker run")));
        assert!(s.button.is_some() && !s.measuring);

        // Images: those no container uses; with everything, also those of stopped containers only
        assert_eq!(images(Prune::Images), ["node:20-alpine", "postgres:15", "redis:7", "2 senza nome"].map(String::from).map(|n| if crate::i18n::italian() { n } else { n.replace("senza nome", "untagged") }));
        let all = images(Prune::All);
        assert!(all.contains(&"minio:latest".to_string()) && !all.contains(&"shop-api:dev".to_string()) && !all.contains(&"postgres:16".to_string()));

        // Networks: those of no running container; a stopped one on them won't start again
        let s = d.sheet(&Ask::Prune(Prune::Networks)).unwrap();
        assert_eq!(goes(&s), [("network", vec!["lab".to_string(), "old_default".to_string()])]);
        assert!(s.notes.iter().any(|(_, hue, text)| *hue == Hue::Warning && text.contains("minio") && !text.contains("shop-worker-1")));

        // Everything: each kind, in the order they go
        let s = d.sheet(&Ask::Prune(Prune::All)).unwrap();
        let kinds: Vec<&str> = s.goes.iter().map(|g| g.symbol).collect();
        assert_eq!(kinds, ["shippingbox.fill", "network", "square.stack.3d.up.fill", "hammer.fill"]);
        assert!(s.notes.iter().any(|(_, hue, _)| *hue == Hue::Warning));

        // One container: running, it's stopped first
        let s = d.sheet(&Ask::Remove("shop-db-1".into())).unwrap();
        assert_eq!(s.notes[0].0, "stop.circle.fill");
        assert!(s.notes.iter().any(|(_, hue, text)| *hue == Hue::Keeps && text.contains("postgres:16")));
        assert!(d.sheet(&Ask::Remove("gone".into())).is_none());
    }

    #[test]
    fn answers() {
        let mut d = made_up();
        let measuring = |d: &Docker, kind| d.sheet(&Ask::Prune(kind)).unwrap().measuring;
        // Until the disk is measured again, a clean-up can't be confirmed
        d.sizing = true;
        assert!(measuring(&d, Prune::Images) && measuring(&d, Prune::All));
        assert!(!measuring(&d, Prune::Containers));
        d.sizing = false;
        assert!(!measuring(&d, Prune::Images));
        // Nothing to remove: no button
        let mut empty = made_up();
        empty.snapshot.as_mut().unwrap().containers.retain(Container::up);
        assert!(empty.sheet(&Ask::Prune(Prune::Containers)).unwrap().button.is_none());
        let names = names_in(&(0..40).map(|i| format!("container-{i}")).collect::<Vec<_>>(), &crate::gfx::FontSpec::new("SF Pro", Weight::Medium, 11.0), 196.0, 3);
        // At most three lines, the rest counted
        assert_eq!(names.len(), 3);
        assert!(names[2].ends_with(" more") || names[2].contains(" altri "));
    }
}
