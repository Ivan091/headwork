//! `dash`: the agent graph as a web page (`docs/dashboard.md`, milestone D2), served on loopback only and read only.
//!
//! [`route`] turns a request into a [`Route`] (pure: the method, the path and query, and the `Host` header, which must
//! name this loopback port so a page elsewhere cannot reach the server through DNS rebinding); [`answer`] reads the
//! store through a read-only connection and makes the response. The page and its vendored scripts are built in.

use std::fmt::Write as _;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use headwork_core::graph::{Edge, Node, Shape, task_node};
use headwork_store::{SpendOf, Store, TaskId};

use crate::ops::install::Stamp;
use serde::Serialize;

use crate::ops::graph::{GraphQuery, Which, graph};

/// The port `headwork dash` listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 8740;
/// How long a probe waits for the port to answer.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);
/// How many requests are answered at once.
const WORKERS: usize = 4;
/// How many live streams may be open at once; each holds a thread.
const STREAMS_MAX: usize = 8;
/// How often a stream looks for new events.
const STREAM_POLL: std::time::Duration = std::time::Duration::from_secs(1);
/// After how many quiet polls a stream sends a keep-alive line.
const STREAM_PINGS_EVERY: u32 = 15;
/// How often the server looks at the installed program.
const BUILD_POLL: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a replacement server keeps trying to take the port from the one it replaces.
const TAKEOVER_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a replacement server waits between tries.
const TAKEOVER_STEP: std::time::Duration = std::time::Duration::from_millis(250);

/// A file the page loads, built into the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    /// Its path under `/`.
    pub path: &'static str,
    /// Its media type.
    pub content_type: &'static str,
    /// Its bytes.
    pub body: &'static str,
}

/// The page, its script and style, and the vendored graph library (`dash/VENDOR.md`).
pub const ASSETS: &[Asset] = &[
    Asset {
        path: "/",
        content_type: "text/html; charset=utf-8",
        body: include_str!("../../../../dash/index.html"),
    },
    Asset {
        path: "/app.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../../../../dash/app.js"),
    },
    Asset {
        path: "/style.css",
        content_type: "text/css; charset=utf-8",
        body: include_str!("../../../../dash/style.css"),
    },
    Asset {
        path: "/vendor/cytoscape.min.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../../../../dash/vendor/cytoscape.min.js"),
    },
    Asset {
        path: "/vendor/layout-base.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../../../../dash/vendor/layout-base.js"),
    },
    Asset {
        path: "/vendor/cose-base.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../../../../dash/vendor/cose-base.js"),
    },
    Asset {
        path: "/vendor/cytoscape-fcose.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../../../../dash/vendor/cytoscape-fcose.js"),
    },
    Asset {
        path: "/untangle.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../../../../dash/untangle.js"),
    },
];

/// What a request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// A built-in file.
    Asset(&'static Asset),
    /// The graph: `/api/graph?at=<time>&scope=<live|all|task:<name>>`.
    Graph(GraphQuery),
    /// One node's details: `/api/node/<percent-encoded id>`.
    Node(String),
    /// The events after an id, oldest first: `/api/events?since=<id>&limit=<n>`.
    Events {
        /// The last event id the page has.
        since: i64,
        /// At most this many.
        limit: u32,
    },
    /// The span of the event log: `/api/range`.
    Range,
    /// The gate slots of each class, holders and waiters: `/api/gates`.
    Gates,
    /// Server-sent events: a `change` naming the newest event id whenever the log grows: `/api/stream`.
    Stream,
    /// Anything else.
    NotFound,
    /// A method other than `GET`.
    MethodNotAllowed,
    /// A `Host` header that does not name this server, or none.
    BadHost,
    /// A query that does not parse.
    BadRequest(String),
}

/// Whether something already listens on the loopback `port`.
#[must_use]
pub fn is_serving(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &SocketAddr::new(IpAddr::from([127, 0, 0, 1]), port),
        PROBE_TIMEOUT,
    )
    .is_ok()
}

/// Asks the system to open `url` in the default browser; a failure only means the user opens it by hand.
pub fn open_in_browser(url: &str) {
    let launched = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    drop(launched);
}

/// Starts the dashboard by default.
///
/// When `[dash]` is `enabled` and nothing listens on `port`, starts a detached `headwork dash` that outlives the caller
/// and, with `open`, opens the page. A page already served is left alone. Returns whether a server was started;
/// a failure to start only means the user runs `headwork dash` by hand.
#[must_use]
pub fn ensure(
    config: &headwork_core::config::DashConfig,
    home: &crate::home::Home,
    exe: &Path,
) -> bool {
    if !config.enabled || is_serving(config.port) {
        return false;
    }
    let mut command = std::process::Command::new(exe);
    command
        .args(["dash", "--port", &config.port.to_string()])
        .env(crate::home::HOME_VAR, home.root())
        .current_dir(home.root());
    if headwork_adapters::proc::spawn_detached(&mut command).is_err() {
        return false;
    }
    if config.open {
        open_in_browser(&url(&DashOptions {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: config.port,
            db: PathBuf::new(),
            takeover: false,
        }));
    }
    true
}

/// The route for a request: its method, its URL (path and query), its `Host` header and the port served.
#[must_use]
pub fn route(method: &str, url: &str, host: Option<&str>, port: u16) -> Route {
    if !host.is_some_and(|host| {
        host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
    }) {
        return Route::BadHost;
    }
    if method != "GET" {
        return Route::MethodNotAllowed;
    }
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    if let Some(asset) = ASSETS.iter().find(|asset| asset.path == path) {
        return Route::Asset(asset);
    }
    if path == "/api/graph" {
        return graph_query(query).map_or_else(Route::BadRequest, Route::Graph);
    }
    if path == "/api/range" {
        return Route::Range;
    }
    if path == "/api/gates" {
        return Route::Gates;
    }
    if path == "/api/stream" {
        return Route::Stream;
    }
    if path == "/api/events" {
        return events_query(query).map_or_else(Route::BadRequest, |(since, limit)| {
            Route::Events { since, limit }
        });
    }
    if let Some(id) = path.strip_prefix("/api/node/") {
        return decode(id).map_or_else(
            || Route::BadRequest("a node id that does not decode".to_owned()),
            Route::Node,
        );
    }
    Route::NotFound
}

fn graph_query(query: &str) -> Result<GraphQuery, String> {
    let mut found = GraphQuery::default();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = decode(value).ok_or_else(|| format!("`{key}` does not decode"))?;
        match key {
            "at" if !value.is_empty() => found.at = Some(value),
            "at" => {}
            "scope" => {
                found.which = match value.as_str() {
                    "live" | "" => Which::Live,
                    "all" => Which::All,
                    other => match other.strip_prefix("task:") {
                        Some(name) if !name.is_empty() => Which::Task(name.to_owned()),
                        _ => {
                            return Err(format!(
                                "unknown scope `{other}`: live, all or task:<name>"
                            ));
                        }
                    },
                };
            }
            other => return Err(format!("unknown parameter `{other}`")),
        }
    }
    Ok(found)
}

/// The most events one `/api/events` answer holds.
const EVENTS_MAX: u32 = 500;

fn events_query(query: &str) -> Result<(i64, u32), String> {
    let mut since = 0;
    let mut limit = EVENTS_MAX;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "since" => {
                since = value
                    .parse()
                    .map_err(|_| format!("`since` is not an event id: {value}"))?;
            }
            "limit" => {
                limit = value
                    .parse::<u32>()
                    .map_err(|_| format!("`limit` is not a count: {value}"))?
                    .clamp(1, EVENTS_MAX);
            }
            other => return Err(format!("unknown parameter `{other}`")),
        }
    }
    Ok((since, limit))
}

/// Percent-decodes a URL component (`+` is a space); none when it is not valid UTF-8 or an escape is broken.
#[must_use]
pub fn decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.bytes();
    while let Some(byte) = rest.next() {
        match byte {
            b'%' => {
                let high = char::from(rest.next()?).to_digit(16)?;
                let low = char::from(rest.next()?).to_digit(16)?;
                bytes.push(u8::try_from(high * 16 + low).ok()?);
            }
            b'+' => bytes.push(b' '),
            other => bytes.push(other),
        }
    }
    String::from_utf8(bytes).ok()
}

/// A response: its status, media type and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The HTTP status.
    pub status: u16,
    /// The media type.
    pub content_type: &'static str,
    /// The body.
    pub body: Vec<u8>,
}

const JSON: &str = "application/json; charset=utf-8";

impl Answer {
    fn json(status: u16, value: &impl Serialize) -> Self {
        let body = serde_json::to_vec(value)
            .unwrap_or_else(|_| b"{\"error\":\"the answer did not serialise\"}".to_vec());
        Self {
            status,
            content_type: JSON,
            body,
        }
    }

    fn error(status: u16, message: &str) -> Self {
        Self::json(status, &serde_json::json!({ "error": message }))
    }
}

/// A node with the edges that touch it and what the store knows about its task.
#[derive(Debug, Clone, Serialize)]
pub struct NodeDetails {
    /// The node, drawn now with every task.
    pub node: Node,
    /// The edges into and out of it.
    pub edges: Vec<Edge>,
    /// For a task, its state changes: `(at, from, to)`, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<(String, String, String)>,
    /// For a task, its newest recap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recap: Option<String>,
    /// For a task (its own), a session or a subagent, the tokens spent by type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend: Option<headwork_core::usage::Tokens>,
}

/// The answer to a route, read from the store at `db` through a read-only connection.
#[must_use]
pub fn answer(db: &Path, route: &Route) -> Answer {
    match route {
        Route::Asset(asset) => Answer {
            status: 200,
            content_type: asset.content_type,
            body: asset.body.as_bytes().to_vec(),
        },
        Route::NotFound => Answer::error(404, "not found"),
        Route::MethodNotAllowed => Answer::error(405, "read only: GET only"),
        Route::BadHost => Answer::error(403, "the Host header must name this loopback server"),
        Route::BadRequest(why) => Answer::error(400, why),
        Route::Graph(query) => match Store::open_read_only(db)
            .map_err(anyhow::Error::from)
            .and_then(|store| graph(&store, query))
        {
            Ok(graph) => Answer::json(200, &graph),
            Err(error) => Answer::error(400, &format!("{error:#}")),
        },
        Route::Events { since, limit } => {
            match Store::open_read_only(db).and_then(|store| store.events_after(*since, *limit)) {
                Ok(events) => Answer::json(200, &events),
                Err(error) => Answer::error(500, &error.to_string()),
            }
        }
        Route::Range => match Store::open_read_only(db).and_then(|store| store.event_range()) {
            Ok(range) => Answer::json(200, &range),
            Err(error) => Answer::error(500, &error.to_string()),
        },
        Route::Gates => match db
            .parent()
            .map(|root| crate::home::Home::at(Some(root.to_path_buf())))
        {
            Some(Ok(home)) => Answer::json(200, &super::gate::status(&home)),
            Some(Err(error)) => Answer::error(500, &format!("{error:#}")),
            None => Answer::error(500, "the store has no home folder"),
        },
        Route::Stream => Answer::error(500, "the stream is served by its own thread"),
        Route::Node(id) => match node_details(db, id) {
            Ok(Some(details)) => Answer::json(200, &details),
            Ok(None) => Answer::error(404, "no such node"),
            Err(error) => Answer::error(500, &format!("{error:#}")),
        },
    }
}

fn node_details(db: &Path, id: &str) -> Result<Option<NodeDetails>> {
    let store = Store::open_read_only(db)?;
    let view = graph(
        &store,
        &GraphQuery {
            at: None,
            which: Which::All,
        },
    )?;
    let Some(node) = view.node(id).cloned() else {
        return Ok(None);
    };
    let edges = view
        .edges
        .iter()
        .filter(|edge| edge.source == id || edge.target == id)
        .cloned()
        .collect();
    let task = id
        .strip_prefix("task:")
        .and_then(|task| task.parse::<i64>().ok())
        .filter(|task| task_node(*task) == id);
    let (history, recap) = match task {
        Some(task) => {
            let facts = store.graph_facts()?;
            let history = facts
                .transitions
                .iter()
                .filter(|change| change.key == task)
                .map(|change| {
                    let from = change
                        .from
                        .map_or_else(String::new, |from| from.as_str().to_owned());
                    (change.at.clone(), from, change.to.as_str().to_owned())
                })
                .collect();
            let recap = store.latest_recap(TaskId(task))?.map(|recap| recap.body);
            (history, recap)
        }
        None => (Vec::new(), None),
    };
    let whose = match node.shape {
        Shape::Lead(_) | Shape::Worker(_) | Shape::Part(_) => task.map(SpendOf::Task),
        Shape::Subagent(_) => id.strip_prefix("agent:").map(SpendOf::Agent),
        Shape::User
        | Shape::Orchestrator(_)
        | Shape::Helper
        | Shape::Review(_)
        | Shape::ReviewNode(_)
        | Shape::Job(_)
        | Shape::Pr(_)
        | Shape::Ask(_)
        | Shape::External => None,
    };
    let spend = whose.map(|whose| store.spend_of(whose)).transpose()?;
    Ok(Some(NodeDetails {
        node,
        edges,
        history,
        recap,
        spend,
    }))
}

/// The installed program beside the one running: a hot-reload install moves the running copy aside and puts the new
/// build under the usual name in the same folder.
fn installed_beside(running: &Path) -> Option<PathBuf> {
    running
        .parent()
        .map(|dir| dir.join(crate::ops::install::program_name()))
}

/// Whether the installed build differs from the one the server started with; an unreadable file counts as unchanged.
#[must_use]
pub fn build_changed(started: Option<Stamp>, now: Option<Stamp>) -> bool {
    matches!((started, now), (Some(started), Some(now)) if started != now)
}

fn build_stamp(installed: Option<&Path>) -> Option<Stamp> {
    installed.and_then(|path| Stamp::of(path).ok())
}

fn watch_build(installed: PathBuf, started: Option<Stamp>, port: u16) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(BUILD_POLL);
            if !build_changed(started, build_stamp(Some(&installed))) {
                continue;
            }
            let mut command = std::process::Command::new(&installed);
            command.args(["dash", "--port", &port.to_string(), "--takeover"]);
            if let Ok(home) = crate::home::Home::locate() {
                command.env(crate::home::HOME_VAR, home.root());
            }
            if headwork_adapters::proc::spawn_detached(&mut command).is_ok() {
                std::process::exit(0);
            }
        }
    });
}

/// Where and how `dash` listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashOptions {
    /// The address to bind: loopback only.
    pub bind: IpAddr,
    /// The port.
    pub port: u16,
    /// The store file.
    pub db: PathBuf,
    /// Whether this server replaces one an update outdated: it keeps trying to bind the port for a while.
    pub takeover: bool,
}

fn bind(address: SocketAddr, takeover: bool) -> Result<tiny_http::Server> {
    let deadline = std::time::Instant::now() + TAKEOVER_WAIT;
    loop {
        match tiny_http::Server::http(address) {
            Ok(server) => return Ok(server),
            Err(error) if !takeover || std::time::Instant::now() >= deadline => {
                return Err(anyhow!("listening on {address}: {error}"));
            }
            Err(_) => std::thread::sleep(TAKEOVER_STEP),
        }
    }
}

/// The page's address.
#[must_use]
pub fn url(options: &DashOptions) -> String {
    format!("http://127.0.0.1:{}/", options.port)
}

/// Serves the page until the process ends, writing its address to `out` first.
///
/// # Errors
///
/// Fails when the address is not loopback, the store does not open read-only, or the port cannot be bound.
pub fn serve(options: &DashOptions, out: &mut impl Write) -> Result<()> {
    if !options.bind.is_loopback() {
        bail!(
            "headwork dash serves on loopback only; {} is not",
            options.bind
        );
    }
    drop(
        Store::open_read_only(&options.db)
            .with_context(|| format!("opening {} read only", options.db.display()))?,
    );
    let address = SocketAddr::new(options.bind, options.port);
    let server = Arc::new(bind(address, options.takeover)?);
    let running = std::env::current_exe().ok();
    let installed = running.as_deref().and_then(installed_beside);
    let started = build_stamp(installed.as_deref());
    let build = started.map_or_else(|| "unknown".to_owned(), |stamp| stamp.to_string());
    if let Some(installed) = installed {
        watch_build(installed, started, options.port);
    }
    let mut line = String::new();
    let _ = write!(
        line,
        "headwork dash: {} (read only; Ctrl+C stops it)",
        url(options)
    );
    writeln!(out, "{line}")?;
    out.flush()?;
    let streams = Arc::new(AtomicUsize::new(0));
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let server = Arc::clone(&server);
            let db = options.db.clone();
            let port = options.port;
            let streams = Arc::clone(&streams);
            let build = build.clone();
            std::thread::spawn(move || {
                for request in server.incoming_requests() {
                    handle(request, &db, port, &streams, &build);
                }
            })
        })
        .collect();
    for worker in workers {
        worker
            .join()
            .map_err(|_| anyhow!("a dash worker stopped"))?;
    }
    Ok(())
}

fn security_headers(content_type: &str) -> Vec<tiny_http::Header> {
    [
        ("Content-Type", content_type),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Content-Security-Policy",
            "default-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'",
        ),
    ]
    .into_iter()
    .filter_map(|(field, value)| {
        tiny_http::Header::from_bytes(field.as_bytes(), value.as_bytes()).ok()
    })
    .collect()
}

/// A live stream's body: a `change` line now and whenever the event log grows, a keep-alive comment line when it is
/// quiet; it ends when the store cannot be read or the page goes away.
struct ChangeStream {
    build: String,
    db: PathBuf,
    last: Option<i64>,
    quiet: u32,
    pending: Vec<u8>,
    streams: Arc<AtomicUsize>,
}

impl ChangeStream {
    fn next_lines(&mut self) -> std::io::Result<()> {
        loop {
            let newest = Store::open_read_only(&self.db)
                .and_then(|store| store.last_event_id())
                .map_err(std::io::Error::other)?;
            if self.last != Some(newest) {
                let hello = if self.last.is_none() {
                    format!("event: build\ndata: {}\n\n", self.build)
                } else {
                    String::new()
                };
                self.last = Some(newest);
                self.quiet = 0;
                self.pending = format!("{hello}event: change\ndata: {newest}\n\n").into_bytes();
                return Ok(());
            }
            self.quiet += 1;
            if self.quiet >= STREAM_PINGS_EVERY {
                self.quiet = 0;
                self.pending = b": ping\n\n".to_vec();
                return Ok(());
            }
            std::thread::sleep(STREAM_POLL);
        }
    }
}

impl ChangeStream {
    /// The next lines to send: at once the first time, then when the log grows or a keep-alive is due.
    fn next_chunk(&mut self) -> std::io::Result<Vec<u8>> {
        self.next_lines()?;
        Ok(std::mem::take(&mut self.pending))
    }
}

impl Drop for ChangeStream {
    fn drop(&mut self) {
        self.streams.fetch_sub(1, Ordering::SeqCst);
    }
}

fn open_stream(request: tiny_http::Request, db: &Path, streams: &Arc<AtomicUsize>, build: &str) {
    if streams.fetch_add(1, Ordering::SeqCst) >= STREAMS_MAX {
        streams.fetch_sub(1, Ordering::SeqCst);
        let reply = Answer::error(503, "too many live views are open");
        let mut response =
            tiny_http::Response::from_data(reply.body).with_status_code(reply.status);
        for header in security_headers(reply.content_type) {
            response.add_header(header);
        }
        drop(request.respond(response));
        return;
    }
    let body = ChangeStream {
        build: build.to_owned(),
        db: db.to_path_buf(),
        last: None,
        quiet: 0,
        pending: Vec::new(),
        streams: Arc::clone(streams),
    };
    std::thread::spawn(move || {
        let mut body = body;
        let mut socket = request.into_writer();
        let mut head = String::from("HTTP/1.1 200 OK\r\nConnection: close\r\n");
        for header in security_headers("text/event-stream") {
            let _ = write!(head, "{}: {}\r\n", header.field, header.value);
        }
        head.push_str("\r\n");
        if socket
            .write_all(head.as_bytes())
            .and_then(|()| socket.flush())
            .is_err()
        {
            return;
        }
        while let Ok(chunk) = body.next_chunk() {
            if socket
                .write_all(&chunk)
                .and_then(|()| socket.flush())
                .is_err()
            {
                return;
            }
        }
    });
}

fn handle(
    request: tiny_http::Request,
    db: &Path,
    port: u16,
    streams: &Arc<AtomicUsize>,
    build: &str,
) {
    let host = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Host"))
        .map(|header| header.value.as_str().to_owned());
    let found = route(
        request.method().as_str(),
        request.url(),
        host.as_deref(),
        port,
    );
    if found == Route::Stream {
        open_stream(request, db, streams, build);
        return;
    }
    let reply = answer(db, &found);
    let mut response = tiny_http::Response::from_data(reply.body).with_status_code(reply.status);
    for header in security_headers(reply.content_type) {
        response.add_header(header);
    }
    drop(request.respond(response));
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]
mod tests {
    use super::*;

    #[test]
    fn a_bound_port_is_serving_and_a_free_one_is_not() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(is_serving(port), "bound port");
        drop(listener);
        assert!(!is_serving(port), "released port");
    }

    #[test]
    fn a_disabled_dash_is_never_started() {
        let home = crate::home::Home::at(Some(std::env::temp_dir())).unwrap();
        let config = headwork_core::config::DashConfig {
            enabled: false,
            open: true,
            port: 1,
        };
        assert!(!ensure(&config, &home, Path::new("headwork")), "disabled");
    }

    const PORT: u16 = 8740;
    const HOST: Option<&str> = Some("127.0.0.1:8740");

    #[test]
    fn routes_take_the_page_the_api_and_refuse_the_rest() {
        let table: [(&str, &str, Option<&str>, Route); 9] = [
            ("GET", "/", HOST, Route::Asset(&ASSETS[0])),
            (
                "GET",
                "/app.js?v=1",
                Some("localhost:8740"),
                Route::Asset(&ASSETS[1]),
            ),
            (
                "GET",
                "/api/graph",
                HOST,
                Route::Graph(GraphQuery::default()),
            ),
            (
                "GET",
                "/api/graph?scope=task%3Aheadwork-lead&at=2026-11-23",
                HOST,
                Route::Graph(GraphQuery {
                    at: Some("2026-11-23".to_owned()),
                    which: Which::Task("headwork-lead".to_owned()),
                }),
            ),
            (
                "GET",
                "/api/node/task%3A12",
                HOST,
                Route::Node("task:12".to_owned()),
            ),
            ("GET", "/etc/passwd", HOST, Route::NotFound),
            ("POST", "/api/graph", HOST, Route::MethodNotAllowed),
            ("GET", "/", Some("evil.example:8740"), Route::BadHost),
            ("GET", "/", None, Route::BadHost),
        ];
        for (method, url, host, expected) in table {
            assert_eq!(
                route(method, url, host, PORT),
                expected,
                "{method} {url} {host:?}"
            );
        }
        assert!(
            matches!(
                route("GET", "/api/graph?scope=nope", HOST, PORT),
                Route::BadRequest(_)
            ),
            "bad scope"
        );
        assert!(
            matches!(
                route("GET", "/api/graph?x=1", HOST, PORT),
                Route::BadRequest(_)
            ),
            "unknown parameter"
        );
        assert!(
            matches!(
                route("GET", "/api/node/%zz", HOST, PORT),
                Route::BadRequest(_)
            ),
            "broken escape"
        );
        assert_eq!(
            route("GET", "/", Some("127.0.0.1:9999"), PORT),
            Route::BadHost,
            "another port"
        );
    }

    #[test]
    fn decode_reads_percent_escapes_and_refuses_broken_ones() {
        let table = [
            ("task%3A12", Some("task:12")),
            ("a+b", Some("a b")),
            ("%C5%BE", Some("ž")),
            ("external%3A00ff", Some("external:00ff")),
            ("%", None),
            ("%4", None),
            ("%FF", None),
        ];
        for (text, decoded) in table {
            assert_eq!(decode(text).as_deref(), decoded, "{text}");
        }
    }

    #[test]
    fn the_page_writes_agent_text_only_as_text_and_encodes_every_id_it_fetches() {
        let app = ASSETS
            .iter()
            .find(|asset| asset.path == "/app.js")
            .unwrap()
            .body;
        for banned in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
        ] {
            assert!(!app.contains(banned), "app.js uses {banned}");
        }
        let built = |text: &str| text.contains('+');
        for (at, _) in app.match_indices("fetch(") {
            let rest = app.get(at + "fetch(".len()..).unwrap_or_default();
            let argument = rest.split([',', ')']).next().unwrap_or_default();
            assert!(
                !built(argument) || argument.contains("encodeURIComponent"),
                "a fetch builds its path without encoding: {argument}"
            );
        }
        let graph_url = app
            .split("function graphUrl()")
            .nth(1)
            .and_then(|rest| rest.split("\n  }").next())
            .unwrap();
        for line in graph_url.lines().filter(|line| built(line)) {
            assert!(
                line.contains("encodeURIComponent"),
                "graphUrl adds to the path without encoding: {line}"
            );
        }
        let page = ASSETS[0].body;
        assert!(
            !page.contains("http://") && !page.contains("https://"),
            "no external requests"
        );
    }

    #[test]
    fn answers_carry_status_and_type_without_touching_a_store() {
        let none = Path::new("no-such.db");
        assert_eq!(answer(none, &Route::NotFound).status, 404, "not found");
        assert_eq!(answer(none, &Route::BadHost).status, 403, "bad host");
        assert_eq!(
            answer(none, &Route::MethodNotAllowed).status,
            405,
            "read only"
        );
        let page = answer(none, &Route::Asset(&ASSETS[0]));
        assert_eq!(
            (page.status, page.content_type),
            (200, "text/html; charset=utf-8"),
            "the page"
        );
        let failed = answer(none, &Route::Graph(GraphQuery::default()));
        assert_eq!(
            (failed.status, failed.content_type),
            (400, JSON),
            "a store that does not open"
        );
    }

    #[test]
    fn serve_refuses_an_address_off_loopback() {
        let options = DashOptions {
            bind: "0.0.0.0".parse().unwrap(),
            port: 0,
            db: PathBuf::from("x.db"),
            takeover: false,
        };
        let error = serve(&options, &mut Vec::new()).unwrap_err().to_string();
        assert!(error.contains("loopback only"), "{error}");
    }

    #[test]
    fn the_crossing_reducer_is_served_as_script_and_loaded_before_the_page_script() {
        let asset = ASSETS
            .iter()
            .find(|asset| asset.path == "/untangle.js")
            .unwrap();
        assert!(
            asset.content_type.starts_with("text/javascript"),
            "script type"
        );
        let page = ASSETS[0].body;
        let tool = page.find("/untangle.js").unwrap();
        let app = page.find("/app.js").unwrap();
        assert!(tool < app, "untangle.js loads before app.js");
    }

    #[test]
    fn done_items_are_hidden_on_first_load() {
        let page = ASSETS[0].body;
        assert!(
            page.contains(r#"id="hide-done" checked"#),
            "the hide done box starts checked"
        );
    }

    #[test]
    fn vendored_files_match_their_checksums() {
        use sha2::{Digest, Sha256};

        let table = include_str!("../../../../dash/VENDOR.md");
        let vendored: Vec<&Asset> = ASSETS
            .iter()
            .filter(|asset| asset.path.starts_with("/vendor/"))
            .collect();
        assert_eq!(vendored.len(), 4, "four vendored files");
        for asset in vendored {
            let file = asset.path.trim_start_matches('/');
            let row = table
                .lines()
                .find(|line| line.starts_with(&format!("| `{file}`")))
                .unwrap();
            let digest = Sha256::digest(asset.body.as_bytes());
            let hex = digest.iter().fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
            assert!(row.contains(&hex), "{file}: {hex} is not in VENDOR.md");
        }
    }

    #[test]
    fn live_and_replay_routes_take_their_queries() {
        let table = [
            ("/api/range", Route::Range),
            ("/api/gates", Route::Gates),
            ("/api/stream", Route::Stream),
            (
                "/api/events",
                Route::Events {
                    since: 0,
                    limit: EVENTS_MAX,
                },
            ),
            (
                "/api/events?since=42&limit=10",
                Route::Events {
                    since: 42,
                    limit: 10,
                },
            ),
            (
                "/api/events?limit=100000",
                Route::Events {
                    since: 0,
                    limit: EVENTS_MAX,
                },
            ),
        ];
        for (url, expected) in table {
            assert_eq!(route("GET", url, HOST, PORT), expected, "{url}");
        }
        for bad in [
            "/api/events?since=x",
            "/api/events?limit=-1",
            "/api/events?what=1",
        ] {
            assert!(
                matches!(route("GET", bad, HOST, PORT), Route::BadRequest(_)),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_change_stream_names_the_newest_event_now_and_when_the_log_grows() {
        use headwork_core::Actor;

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let store = Store::open(&db).unwrap();
        let first = store.last_event_id().unwrap();
        let streams = Arc::new(AtomicUsize::new(1));
        let mut stream = ChangeStream {
            build: "7:9".to_owned(),
            db,
            last: None,
            quiet: 0,
            pending: Vec::new(),
            streams: Arc::clone(&streams),
        };
        let chunk = stream.next_chunk().unwrap();
        assert_eq!(
            chunk,
            format!("event: build\ndata: 7:9\n\nevent: change\ndata: {first}\n\n").into_bytes(),
            "the build and the newest id at once"
        );
        store
            .append_event(Actor::User, None, None, "probe", &serde_json::json!({}))
            .unwrap();
        let chunk = stream.next_chunk().unwrap();
        let next = store.last_event_id().unwrap();
        assert_eq!(
            chunk,
            format!("event: change\ndata: {next}\n\n").into_bytes(),
            "the new id"
        );
        drop(stream);
        assert_eq!(
            streams.load(Ordering::SeqCst),
            0,
            "a closed stream frees its place"
        );
    }

    #[test]
    fn only_a_readable_different_build_counts_as_changed() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one");
        std::fs::write(&one, b"a").unwrap();
        let two = dir.path().join("two");
        std::fs::write(&two, b"longer").unwrap();
        let first = Stamp::of(&one).ok();
        let second = Stamp::of(&two).ok();
        assert!(build_changed(first, second), "another file");
        assert!(!build_changed(first, first), "the same file");
        assert!(!build_changed(first, None), "unreadable now");
        assert!(!build_changed(None, second), "unknown at start");
    }

    #[test]
    fn the_installed_program_sits_beside_the_running_one() {
        let installed = installed_beside(Path::new("bin/headwork.old-5.exe")).unwrap();
        assert_eq!(
            installed,
            Path::new("bin").join(crate::ops::install::program_name())
        );
    }

    #[test]
    fn the_range_and_events_answer_from_a_store_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        drop(Store::open(&db).unwrap());
        let range = answer(&db, &Route::Range);
        assert_eq!(
            range.status,
            200,
            "{}",
            String::from_utf8_lossy(&range.body)
        );
        let events = answer(&db, &Route::Events { since: 0, limit: 5 });
        let parsed: serde_json::Value = serde_json::from_slice(&events.body).unwrap();
        assert!(parsed.is_array(), "a list: {parsed}");
    }

    #[test]
    fn the_gates_answer_lists_holders_and_waiters_of_a_fixture_folder() {
        let home = std::env::temp_dir().join(format!("hw-dash-gates-{}", std::process::id()));
        let slots = home.join("slots");
        let slot = slots.join("cpu").join("slot-1");
        std::fs::create_dir_all(&slot).unwrap();
        let pid = std::process::id();
        let owner = format!(
            "pid={pid}\nnative=1\nclass=cpu\ntask=demo\ncommand=cargo test\nepoch={}\n",
            headwork_adapters::gate::epoch_now()
        );
        std::fs::write(slot.join("owner"), &owner).unwrap();
        std::fs::write(slots.join("cpu").join(format!("wait-{pid}")), &owner).unwrap();
        let dir = slots.to_string_lossy().replace(char::from(92), "/");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
        let base = std::fs::read_to_string(fixture).unwrap();
        std::fs::write(
            home.join("config.toml"),
            format!("{base}\n[gates]\ndir = \"{dir}\"\n"),
        )
        .unwrap();
        let reply = answer(&home.join("state.db"), &Route::Gates);
        assert_eq!(reply.status, 200, "gates");
        let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        let cpu = body["classes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|class| class["class"] == "cpu")
            .unwrap();
        assert_eq!(cpu["used"], 1, "used");
        assert!(cpu["count"].as_u64().unwrap() >= 1, "count");
        let holder = &cpu["holders"][0];
        assert_eq!(holder["task"], "demo", "holder task");
        assert_eq!(holder["command"], "cargo test", "holder command");
        assert_eq!(holder["slot"], 1, "holder slot");
        assert!(
            holder["age_s"].is_u64() && holder["stale"] == false && holder["legacy"] == false,
            "holder fields"
        );
        let waiter = &cpu["waiters"][0];
        assert_eq!(waiter["task"], "demo", "waiter task");
        assert!(waiter["age_s"].is_u64(), "waiter age");
        std::fs::remove_dir_all(&home).unwrap();
    }
}
