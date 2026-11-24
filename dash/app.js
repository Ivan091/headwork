"use strict";

(function () {
  const FALLBACK_MS = 3000;
  const PLAY_TICK_MS = 400;
  const PULSE_MS = 1500;
  const RING_MS = 1600;
  const RING_FRAME_MS = 50;
  const toggles = {
    subagent: "show-subagents",
    helper: "show-subagents",
    job: "show-jobs",
    pr: "show-prs",
    review: "show-reviews",
    "review-node": "show-reviews",
    ask: "show-asks",
    user: "show-user",
  };
  const words = {
    green: ["working", "spawning", "live", "starting", "running", "open-pr"],
    red: ["needs_user", "failed", "open-ask", "pending"],
    amber: ["in_review", "requested", "delivered", "rechecking", "asking", "ready", "stopping", "parking", "parked"],
    grey: ["queued", "seen"],
    faded: [
      "closed", "dropped", "merged", "closing", "interrupted", "ended", "gone", "done", "cancelled", "answered",
      "superseded", "expired",
    ],
  };

  const byId = (id) => document.getElementById(id);
  const scopeSelect = byId("scope");
  const projectSelect = byId("project");
  const layoutSelect = byId("layout");
  const searchInput = byId("search");
  const status = byId("status");
  const panel = byId("panel");
  const panelTitle = byId("panel-title");
  const panelBody = byId("panel-body");
  const unfocusButton = byId("unfocus");
  const liveButton = byId("live");
  const playButton = byId("play");
  const speedSelect = byId("speed");
  const slider = byId("slider");
  const instant = byId("instant");
  const replayTaskButton = byId("replay-task");
  const hideDone = byId("hide-done");

  let selected = null;
  let focused = null;
  let loading = false;
  let firstLayout = true;
  let firstGraph = true;
  let replayAt = null;
  let pendingLoad = false;
  let playTimer = null;
  let fallbackTimer = null;

  function colours() {
    const css = getComputedStyle(document.documentElement);
    const read = (name) => css.getPropertyValue(name).trim();
    return {
      bg: read("--bg"),
      text: read("--text"),
      muted: read("--muted"),
      line: read("--line"),
      green: read("--green"),
      red: read("--red"),
      amber: read("--amber"),
      grey: read("--grey"),
      blue: read("--blue"),
      violet: read("--violet"),
      faded: read("--faded"),
    };
  }

  function tone(kind, state) {
    if (kind === "user") {
      return "violet";
    }
    if (kind === "orchestrator") {
      return state === "ended" ? "faded" : "blue";
    }
    let word = state || "";
    if (kind === "pr" && word === "open") {
      word = "open-pr";
    }
    if (kind === "ask" && word === "open") {
      word = "open-ask";
    }
    for (const [name, list] of Object.entries(words)) {
      if (list.includes(word)) {
        return name;
      }
    }
    return "grey";
  }

  function isWorking(node) {
    if (node.kind === "orchestrator") {
      return node.state === "live";
    }
    return TASK_KINDS.includes(node.kind) && tone(node.kind, node.state) === "green";
  }

  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

  function ringFrame() {
    const phase = (Date.now() % RING_MS) / RING_MS;
    const working = cy.nodes("[?working]");
    if (reducedMotion.matches) {
      working.style({ "outline-width": 3, "outline-offset": 2, "outline-opacity": 1 });
      return;
    }
    working.style({
      "outline-width": 3,
      "outline-offset": 1 + phase * 7,
      "outline-opacity": 1 - phase,
    });
  }

  function style() {
    const c = colours();
    return [
      {
        selector: "node",
        style: {
          label: "data(shown)",
          color: c.text,
          "font-size": 10,
          "text-valign": "bottom",
          "text-margin-y": 3,
          "text-wrap": "ellipsis",
          "text-max-width": 160,
          "background-color": (node) => c[node.data("tone")] || c.grey,
          width: "data(size)",
          height: "data(size)",
          "border-width": (node) => (node.data("raw") && node.data("raw").badges && node.data("raw").badges.findings ? 3 : 0),
          "border-color": c.red,
        },
      },
      { selector: "node[kind = 'user']", style: { shape: "star", width: 34, height: 34 } },
      { selector: "node[kind = 'orchestrator']", style: { shape: "hexagon", width: 40, height: 40, "font-size": 12 } },
      { selector: "node[kind = 'lead']", style: { shape: "ellipse", width: 30, height: 30, "font-size": 11 } },
      { selector: "node[kind = 'worker'], node[kind = 'part']", style: { shape: "ellipse" } },
      { selector: "node[kind = 'subagent'], node[kind = 'helper']", style: { "font-size": 8 } },
      { selector: "node[kind = 'job']", style: { shape: "round-rectangle", width: 16, height: 12 } },
      { selector: "node[kind = 'pr']", style: { shape: "tag", width: 16, height: 14 } },
      { selector: "node[kind = 'review']", style: { shape: "diamond", width: 20, height: 20 } },
      { selector: "node[kind = 'review-node']", style: { shape: "diamond", width: 10, height: 10, "font-size": 8 } },
      { selector: "node[kind = 'ask']", style: { shape: "round-triangle", width: 20, height: 20 } },
      { selector: "node[kind = 'external']", style: { shape: "round-octagon", "background-color": c.grey } },
      {
        selector: "edge",
        style: {
          width: (edge) => Math.min(1 + Math.log2(edge.data("weight") || 1), 6),
          "line-color": c.line,
          "target-arrow-color": c.line,
          "target-arrow-shape": "triangle",
          "arrow-scale": 0.7,
          "curve-style": "bezier",
          label: "data(shown)",
          "font-size": 8,
          color: c.muted,
          "text-rotation": "autorotate",
          "text-background-color": c.bg,
          "text-background-opacity": 0.8,
          "text-background-padding": 1,
        },
      },
      { selector: "edge[kind = 'message']", style: { "line-color": c.blue, "target-arrow-color": c.blue } },
      { selector: "edge[kind = 'report']", style: { "line-color": c.green, "target-arrow-color": c.green } },
      { selector: "edge[kind = 'asked']", style: { "line-color": c.red, "target-arrow-color": c.red } },
      { selector: "edge[kind = 'depends-on']", style: { "line-style": "dashed" } },
      { selector: "edge[kind = 'part-of']", style: { "line-style": "dotted" } },
      {
        selector: "node[?working]",
        style: { "outline-color": c.green, "outline-width": 3, "outline-offset": 2, "outline-opacity": 1 },
      },
      { selector: ".hidden", style: { display: "none" } },
      { selector: ".faded", style: { opacity: 0.12 } },
      { selector: "node:selected", style: { "border-width": 3, "border-color": c.violet } },
      {
        selector: "edge.pulse",
        style: {
          "line-color": c.violet,
          "target-arrow-color": c.violet,
          width: 6,
          "z-index": 10,
          "transition-property": "line-color, target-arrow-color, width",
          "transition-duration": "0.4s",
        },
      },
    ];
  }

  cytoscape.use(cytoscapeFcose);
  const cy = cytoscape({
    container: byId("graph"),
    style: style(),
    wheelSensitivity: 0.3,
    minZoom: 0.05,
    maxZoom: 4,
    pixelRatio: crispPixelRatio(),
  });

  function crispPixelRatio() {
    return Math.max(2, Math.ceil(window.devicePixelRatio || 1));
  }

  function watchPixelRatio() {
    const query = window.matchMedia("(resolution: " + (window.devicePixelRatio || 1) + "dppx)");
    query.addEventListener(
      "change",
      () => {
        cy.renderer().redrawHint("eles", true);
        cy.resize();
        cy.renderer().forcedPixelRatio = crispPixelRatio();
        cy.forceRender();
        watchPixelRatio();
      },
      { once: true },
    );
  }
  watchPixelRatio();

  function graphUrl() {
    let url = "/api/graph?scope=" + encodeURIComponent(scopeSelect.value);
    if (replayAt !== null) {
      url += "&at=" + encodeURIComponent(new Date(replayAt).toISOString());
    }
    return url;
  }

  const TASK_KINDS = ["lead", "worker", "part"];
  const OWNED_EDGES = ["opened", "called", "delivered", "reviews", "ran"];

  function projectsOf(nodes, edges) {
    const projects = new Map();
    for (const node of nodes) {
      if (TASK_KINDS.includes(node.kind) && node.details && node.details.project) {
        projects.set(node.id, node.details.project);
      }
    }
    for (let pass = 0; pass < 6; pass += 1) {
      let grew = false;
      for (const edge of edges) {
        if (!OWNED_EDGES.includes(edge.kind)) {
          continue;
        }
        const from = projects.get(edge.source);
        const to = projects.get(edge.target);
        if (from && !to) {
          projects.set(edge.target, from);
          grew = true;
        } else if (to && !from) {
          projects.set(edge.source, to);
          grew = true;
        }
      }
      if (!grew) {
        break;
      }
    }
    return new Map(nodes.map((node) => [node.id, projects.get(node.id) || ""]));
  }

  function shortCount(tokens) {
    if (tokens >= 1e9) {
      return (tokens / 1e9).toFixed(1) + "G";
    }
    if (tokens >= 1e6) {
      return (tokens / 1e6).toFixed(1) + "M";
    }
    if (tokens >= 1e3) {
      return (tokens / 1e3).toFixed(1) + "k";
    }
    return String(tokens);
  }

  function shownLabel(node) {
    const badges = node.badges || {};
    let label = node.label;
    if (TASK_KINDS.includes(node.kind) && node.state) {
      label += " · " + node.state;
    }
    if (node.kind === "user" && badges.asks) {
      label += " · " + badges.asks + " ask" + (badges.asks > 1 ? "s" : "") + " waiting";
    }
    if (TASK_KINDS.includes(node.kind) && badges.findings) {
      label += " · ⚠ " + badges.findings;
    }
    if ((node.kind === "review" || node.kind === "review-node") && badges.findings) {
      label += " · " + badges.findings + " finding" + (badges.findings > 1 ? "s" : "");
      if (badges.open) {
        label += " (" + badges.open + " open)";
      }
    }
    if (byId("by-cost").checked && node.tokens) {
      label += " · " + shortCount(node.tokens);
    }
    return label;
  }

  function sizeOf(node, base) {
    if (!byId("by-cost").checked || !node.tokens) {
      return base;
    }
    return Math.min(base + 8 * Math.log10(1 + node.tokens / 1000), 90);
  }

  function apply(graph) {
    const projects = projectsOf(graph.nodes, graph.edges);
    const known = new Set(graph.nodes.map((node) => node.id));
    const before = new Map(cy.edges().map((edge) => [edge.id(), edge.data("raw") ? edge.data("raw").at : null]));
    let changed = false;
    cy.batch(() => {
      for (const element of cy.elements()) {
        const id = element.id();
        const still = element.isNode() ? known.has(id) : graph.edges.some((edge) => edge.id === id);
        if (!still) {
          element.remove();
          changed = true;
        }
      }
      for (const node of graph.nodes) {
        const data = {
          id: node.id,
          label: node.label,
          shown: shownLabel(node),
          kind: node.kind,
          state: node.state || "",
          tone: tone(node.kind, node.state),
          working: isWorking(node),
          size: sizeOf(node, 22),
          project: projects.get(node.id) || "",
          raw: node,
        };
        const existing = cy.getElementById(node.id);
        if (existing.nonempty()) {
          existing.data(data);
        } else {
          cy.add({ group: "nodes", data });
          changed = true;
        }
      }
      for (const edge of graph.edges) {
        const data = {
          id: edge.id,
          source: edge.source,
          target: edge.target,
          kind: edge.kind,
          weight: edge.weight,
          shown: edge.label ? edge.label + (edge.weight > 1 ? " ×" + edge.weight : "") : edge.weight > 1 ? "×" + edge.weight : "",
          raw: edge,
        };
        const existing = cy.getElementById(edge.id);
        if (existing.nonempty()) {
          existing.data(data);
        } else {
          cy.add({ group: "edges", data });
          changed = true;
        }
      }
    });
    refreshProjects(projects);
    filter();
    if (changed) {
      layout();
    }
    if (!firstGraph) {
      for (const edge of graph.edges) {
        const talk = edge.kind === "message" || edge.kind === "report" || edge.kind === "asked" || edge.kind === "spawned";
        const was = before.get(edge.id);
        if (talk && edge.at && was !== edge.at) {
          cy.getElementById(edge.id).flashClass("pulse", PULSE_MS);
        }
      }
    }
    firstGraph = false;
    status.classList.remove("error");
    const when = graph.at ? "at " + new Date(graph.at).toLocaleString() : "updated " + new Date().toLocaleTimeString();
    status.textContent = when + " · " + graph.nodes.length + " nodes";
    if (graph.skipped && graph.skipped.length) {
      status.textContent += " · " + graph.skipped.length + " events skipped";
    }
    if (selected && cy.getElementById(selected).empty()) {
      closePanel();
    }
  }

  function refreshProjects(projects) {
    const names = Array.from(new Set(Array.from(projects.values()).filter(Boolean))).sort();
    const current = projectSelect.value;
    const have = Array.from(projectSelect.options).map((option) => option.value).filter(Boolean);
    if (names.join("|") === have.join("|")) {
      return;
    }
    while (projectSelect.options.length > 1) {
      projectSelect.remove(1);
    }
    for (const name of names) {
      const option = document.createElement("option");
      option.value = name;
      option.textContent = name;
      projectSelect.appendChild(option);
    }
    projectSelect.value = names.includes(current) ? current : "";
  }

  function filter() {
    const project = projectSelect.value;
    const hiddenSubagents = new Map();
    cy.batch(() => {
      for (const node of cy.nodes()) {
        const toggle = toggles[node.data("kind")];
        const offByKind = toggle && !byId(toggle).checked;
        const offByProject = project && node.data("project") && node.data("project") !== project;
        const kind = node.data("kind");
        const done = hideDone.checked && kind !== "user" && kind !== "orchestrator" && tone(kind, node.data("state")) === "faded";
        node.toggleClass("hidden", Boolean(offByKind || offByProject || done));
      }
      for (const edge of cy.edges()) {
        const hidden = edge.source().hasClass("hidden") || edge.target().hasClass("hidden");
        edge.toggleClass("hidden", hidden);
        if (edge.data("kind") === "called" && edge.target().hasClass("hidden")) {
          const caller = edge.source().id();
          hiddenSubagents.set(caller, (hiddenSubagents.get(caller) || 0) + 1);
        }
      }
      for (const node of cy.nodes()) {
        const count = hiddenSubagents.get(node.id());
        const base = shownLabel(node.data("raw"));
        node.data("shown", count ? base + " (" + count + " subagent" + (count > 1 ? "s" : "") + ")" : base);
      }
    });
  }

  function untangleVisible(visible) {
    const tool = window.HeadworkUntangle;
    if (!tool) {
      return;
    }
    try {
      const nodes = visible.nodes();
      const ids = nodes.map((node) => node.id());
      const positions = {};
      nodes.forEach((node) => {
        positions[node.id()] = node.position();
      });
      const edges = visible.edges().map((edge) => [edge.source().id(), edge.target().id()]);
      const result = tool.untangle(ids, edges, positions, tool.DEFAULTS);
      if (result.crossings < 0) {
        return;
      }
      cy.batch(() => {
        nodes.forEach((node) => node.position(result.positions[node.id()]));
      });
    } catch (error) {
      status.textContent = "untangle failed: " + error.message;
    }
  }

  function layout() {
    const visible = cy.elements().not(".hidden");
    const force = layoutSelect.value !== "breadthfirst";
    const options = !force
        ? { name: "breadthfirst", directed: true, roots: "#orchestrator", spacingFactor: 1.2, animate: false }
        : {
            name: "fcose",
            quality: "default",
            randomize: firstLayout,
            animate: false,
            nodeDimensionsIncludeLabels: true,
            nodeRepulsion: 6000,
            idealEdgeLength: 70,
            packComponents: true,
          };
    const before = new Map();
    visible.nodes().forEach((node) => before.set(node.id(), { x: node.position("x"), y: node.position("y") }));
    visible.layout(options).run();
    if (force) {
      untangleVisible(visible);
    }
    if (force && !firstLayout) {
      visible.nodes().forEach((node) => {
        const from = before.get(node.id());
        if (!from) {
          return;
        }
        const to = { x: node.position("x"), y: node.position("y") };
        node.position(from);
        node.animate({ position: to }, { duration: 400 });
      });
    }
    if (firstLayout) {
      cy.fit(visible, 30);
      firstLayout = false;
    }
  }

  async function load() {
    if (loading) {
      pendingLoad = true;
      return;
    }
    loading = true;
    pendingLoad = false;
    try {
      const response = await fetch(graphUrl(), { cache: "no-store" });
      const body = await response.json();
      if (!response.ok) {
        throw new Error(body.error || response.statusText);
      }
      apply(body);
    } catch (error) {
      status.classList.add("error");
      status.textContent = "cannot read the graph: " + error.message;
    } finally {
      loading = false;
      if (pendingLoad) {
        load();
      }
    }
  }

  async function loadRange() {
    try {
      const response = await fetch("/api/range", { cache: "no-store" });
      const range = await response.json();
      if (!response.ok || !range.first_at) {
        return;
      }
      slider.min = String(Date.parse(range.first_at));
      slider.max = String(Date.now());
      if (replayAt === null) {
        slider.value = slider.max;
      }
    } catch (error) {
      status.textContent = "cannot read the log's span: " + error.message;
    }
  }

  function showInstant() {
    instant.textContent = replayAt === null ? "now" : new Date(replayAt).toLocaleString();
    liveButton.classList.toggle("on", replayAt === null);
    playButton.textContent = playTimer ? "pause" : "play";
  }

  function goLive() {
    stopPlaying();
    replayAt = null;
    slider.max = String(Date.now());
    slider.value = slider.max;
    showInstant();
    load();
  }

  function replayFrom(at) {
    replayAt = Math.min(Math.max(at, Number(slider.min)), Date.now() - 1000);
    slider.value = String(replayAt);
    showInstant();
    load();
  }

  function stopPlaying() {
    if (playTimer) {
      clearInterval(playTimer);
      playTimer = null;
    }
    showInstant();
  }

  function play() {
    if (playTimer) {
      stopPlaying();
      return;
    }
    if (replayAt === null) {
      replayFrom(Number(slider.min));
    }
    playTimer = setInterval(() => {
      const next = replayAt + PLAY_TICK_MS * Number(speedSelect.value);
      if (next >= Date.now() - 1000) {
        goLive();
        return;
      }
      replayAt = next;
      slider.value = String(next);
      showInstant();
      if (!loading) {
        load();
      }
    }, PLAY_TICK_MS);
    showInstant();
  }

  function listen() {
    if (!window.EventSource) {
      fallbackTimer = setInterval(() => replayAt === null && load(), FALLBACK_MS);
      return;
    }
    const source = new EventSource("/api/stream");
    let build = null;
    source.addEventListener("build", (event) => {
      if (build !== null && build !== event.data) {
        location.reload();
      }
      build = event.data;
    });
    source.addEventListener("change", () => {
      if (fallbackTimer) {
        clearInterval(fallbackTimer);
        fallbackTimer = null;
      }
      if (replayAt === null) {
        slider.max = String(Date.now());
        slider.value = slider.max;
        load();
      }
    });
    source.addEventListener("error", () => {
      if (!fallbackTimer) {
        fallbackTimer = setInterval(() => replayAt === null && load(), FALLBACK_MS);
      }
    });
  }

  function add(parent, tag, text) {
    const element = document.createElement(tag);
    if (text !== undefined) {
      element.textContent = text;
    }
    parent.appendChild(element);
    return element;
  }

  function pairs(parent, entries) {
    const list = add(parent, "dl");
    for (const [key, value] of entries) {
      if (value === undefined || value === null || value === "") {
        continue;
      }
      add(list, "dt", key);
      add(list, "dd", String(value));
    }
  }

  function labelOf(id) {
    const node = cy.getElementById(id);
    return node.nonempty() ? node.data("label") : id;
  }

  function showDetails(details) {
    const node = details.node;
    panelTitle.textContent = node.label;
    const isTask = TASK_KINDS.includes(node.kind);
    replayTaskButton.hidden = !isTask;
    replayTaskButton.dataset.name = isTask ? node.label : "";
    replayTaskButton.dataset.start = node.start || "";
    panelBody.replaceChildren();
    pairs(panelBody, [
      ["kind", node.kind],
      ["state", node.state],
      ["id", node.id],
      ["started", node.start],
      ["ended", node.end],
    ]);
    const extra = Object.entries(node.details || {});
    if (extra.length) {
      add(panelBody, "h3", "details");
      pairs(panelBody, extra);
    }
    if (details.history && details.history.length) {
      add(panelBody, "h3", "state history");
      const list = add(panelBody, "ul");
      for (const [at, from, to] of details.history) {
        add(list, "li", at + "  " + (from ? from + " → " : "") + to);
      }
    }
    if (details.spend) {
      const spend = details.spend;
      const total = spend.input + spend.cache_write_5m + spend.cache_write_1h + spend.cache_read + spend.output;
      add(panelBody, "h3", "tokens");
      pairs(panelBody, [
        ["total", total.toLocaleString()],
        ["input", spend.input.toLocaleString()],
        ["cache writes (5 min)", spend.cache_write_5m.toLocaleString()],
        ["cache writes (1 h)", spend.cache_write_1h.toLocaleString()],
        ["cache reads", spend.cache_read.toLocaleString()],
        ["output", spend.output.toLocaleString()],
        ["of which thinking", spend.thinking ? spend.thinking.toLocaleString() : ""],
        ["web searches", spend.web_searches || ""],
        ["web fetches", spend.web_fetches || ""],
      ]);
    }
    if (details.recap) {
      add(panelBody, "h3", "newest recap");
      add(panelBody, "pre", details.recap);
    }
    if (details.edges && details.edges.length) {
      add(panelBody, "h3", "connections");
      const list = add(panelBody, "ul");
      for (const edge of details.edges) {
        const outgoing = edge.source === node.id;
        const other = outgoing ? edge.target : edge.source;
        const item = add(list, "li");
        add(item, "span", (outgoing ? edge.kind + " → " : "← " + edge.kind + " "));
        const link = add(item, "span", labelOf(other));
        link.className = "edge-link";
        link.addEventListener("click", () => select(other));
        const notes = [];
        if (edge.weight > 1) {
          notes.push("×" + edge.weight);
        }
        if (edge.label) {
          notes.push("“" + edge.label + "”");
        }
        if (notes.length) {
          add(item, "span", "  " + notes.join(" "));
        }
      }
    }
  }

  async function select(id) {
    const node = cy.getElementById(id);
    if (node.empty()) {
      return;
    }
    selected = id;
    cy.nodes().unselect();
    node.select();
    panel.hidden = false;
    panelTitle.textContent = node.data("label");
    panelBody.replaceChildren();
    add(panelBody, "p", "loading…");
    try {
      const response = await fetch("/api/node/" + encodeURIComponent(id), { cache: "no-store" });
      const body = await response.json();
      if (!response.ok) {
        throw new Error(body.error || response.statusText);
      }
      if (selected === id) {
        showDetails(body);
      }
    } catch (error) {
      panelBody.replaceChildren();
      add(panelBody, "p", "cannot read the node: " + error.message);
    }
  }

  function closePanel() {
    selected = null;
    panel.hidden = true;
    cy.nodes().unselect();
  }

  function focusOn(id) {
    const node = cy.getElementById(id);
    if (node.empty()) {
      return;
    }
    focused = id;
    const near = node.closedNeighborhood().closedNeighborhood();
    const keep = near.union(near.ancestors()).union(near.descendants());
    cy.elements().addClass("faded");
    keep.removeClass("faded");
    keep.connectedEdges().filter((edge) => keep.contains(edge.source()) && keep.contains(edge.target())).removeClass("faded");
    unfocusButton.hidden = false;
    cy.animate({ fit: { eles: keep.not(".hidden"), padding: 40 } }, { duration: 300 });
  }

  function unfocus() {
    focused = null;
    cy.elements().removeClass("faded");
    unfocusButton.hidden = true;
  }

  function search() {
    const text = searchInput.value.trim().toLowerCase();
    if (!text) {
      return;
    }
    const match = cy
      .nodes()
      .not(".hidden")
      .filter((node) => node.data("label").toLowerCase().includes(text) || node.id().toLowerCase().includes(text))
      .first();
    if (match.nonempty()) {
      cy.animate({ center: { eles: match }, zoom: Math.max(cy.zoom(), 1.2) }, { duration: 300 });
      select(match.id());
    } else {
      status.textContent = "nothing matches “" + searchInput.value + "”";
    }
  }

  setInterval(ringFrame, RING_FRAME_MS);
  cy.on("tap", "node", (event) => select(event.target.id()));
  cy.on("tap", (event) => {
    if (event.target === cy) {
      closePanel();
    }
  });
  byId("close").addEventListener("click", closePanel);
  byId("focus").addEventListener("click", () => selected && focusOn(selected));
  unfocusButton.addEventListener("click", unfocus);
  byId("fit").addEventListener("click", () => cy.fit(cy.elements().not(".hidden"), 30));
  searchInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      search();
    }
  });
  scopeSelect.addEventListener("change", () => {
    firstLayout = true;
    load();
  });
  layoutSelect.addEventListener("change", layout);
  projectSelect.addEventListener("change", () => {
    filter();
    layout();
  });
  byId("by-cost").addEventListener("change", () => {
    for (const node of cy.nodes()) {
      const raw = node.data("raw");
      if (raw) {
        node.data("size", sizeOf(raw, 22));
      }
    }
    filter();
  });
  for (const id of new Set([...Object.values(toggles), "hide-done"])) {
    const box = byId(id);
    try {
      const saved = localStorage.getItem(id);
      if (saved !== null) {
        box.checked = saved === "1";
      }
    } catch {
      box.checked = box.checked;
    }
    box.addEventListener("change", () => {
      try {
        localStorage.setItem(id, box.checked ? "1" : "0");
      } catch {
        box.checked = box.checked;
      }
      filter();
      layout();
    });
  }
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    cy.style(style());
    refreshLegend();
  });
  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      if (focused) {
        unfocus();
      } else {
        closePanel();
      }
    }
  });

  liveButton.addEventListener("click", goLive);
  playButton.addEventListener("click", play);
  slider.addEventListener("input", () => {
    stopPlaying();
    replayFrom(Number(slider.value));
  });
  replayTaskButton.addEventListener("click", () => {
    const name = replayTaskButton.dataset.name;
    const start = Date.parse(replayTaskButton.dataset.start);
    if (!name || Number.isNaN(start)) {
      return;
    }
    const value = "task:" + name;
    if (!Array.from(scopeSelect.options).some((option) => option.value === value)) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = name;
      scopeSelect.appendChild(option);
    }
    scopeSelect.value = value;
    firstLayout = true;
    stopPlaying();
    replayFrom(start);
    play();
  });

  const SVG = "http://www.w3.org/2000/svg";
  const SHAPES = {
    star: "M17 1 L20.5 7.5 L28 8.5 L22.5 13.5 L24 21 L17 17.5 L10 21 L11.5 13.5 L6 8.5 L13.5 7.5 Z",
    hexagon: "M10 9 L14 2 L20 2 L24 9 L20 16 L14 16 Z",
    ellipse: "M17 3 A8 6 0 1 0 17.01 3 Z",
    "round-rectangle": "M11 4 H23 V14 H11 Z",
    tag: "M10 4 H21 L25 9 L21 14 H10 Z",
    diamond: "M17 1 L25 9 L17 17 L9 9 Z",
    "round-triangle": "M17 2 L25 16 H9 Z",
    "round-octagon": "M13 2 H21 L26 7 V11 L21 16 H13 L8 11 V7 Z",
  };
  const NODE_ROWS = [
    ["star", "you (the user)", "violet"],
    ["hexagon", "the orchestrator", "blue"],
    ["ellipse", "a task: a lead (larger), worker or part, with its sessions", "green"],
    ["ellipse", "a subagent or helper (smaller)", "green"],
    ["round-rectangle", "a job", "green"],
    ["tag", "a pull request", "green"],
    ["diamond", "a review or review node", "amber"],
    ["round-triangle", "an ask", "red"],
    ["round-octagon", "an outside address", "grey"],
  ];
  const TONE_ROWS = [
    ["green", "working, running, live, open pull request"],
    ["amber", "in review, ready, parked, requested"],
    ["red", "needs you, failed, open ask"],
    ["grey", "queued or not yet seen"],
    ["faded", "closed, merged, ended, done"],
  ];
  const EDGE_ROWS = [
    ["", "#line", "spawned, called, opened, reviews"],
    ["dotted", "#line", "a part hangs off its lead"],
    ["", "#blue", "message between sessions (count on the line)"],
    ["", "#green", "report to the orchestrator"],
    ["", "#red", "ask to the user"],
    ["dashed", "#line", "a task waits for another"],
    ["", "#violet", "talk happening now (pulse)"],
  ];

  function legendSvg(parent, build) {
    const svg = document.createElementNS(SVG, "svg");
    svg.setAttribute("viewBox", "0 0 34 18");
    svg.setAttribute("aria-hidden", "true");
    build(svg);
    parent.appendChild(svg);
  }

  function legendShape(svg, shape, fill, stroke, width) {
    const path = document.createElementNS(SVG, "path");
    path.setAttribute("d", SHAPES[shape]);
    path.setAttribute("fill", fill);
    path.setAttribute("stroke", stroke || "none");
    path.setAttribute("stroke-width", String(width || 0));
    svg.appendChild(path);
  }

  function buildLegend() {
    const box = byId("legend");
    const c = colours();
    const row = (parent, draw, text) => {
      const line = add(parent, "div", "");
      line.className = "row";
      legendSvg(line, draw);
      add(line, "span", text);
    };
    add(box, "h3", "shapes");
    for (const [shape, text, tone] of NODE_ROWS) {
      row(box, (svg) => legendShape(svg, shape, c[tone]), text);
    }
    add(box, "h3", "colours");
    for (const [tone, text] of TONE_ROWS) {
      row(box, (svg) => legendShape(svg, "ellipse", c[tone]), text);
    }
    add(box, "h3", "edges");
    for (const [style, colour, text] of EDGE_ROWS) {
      row(
        box,
        (svg) => {
          const line = document.createElementNS(SVG, "line");
          line.setAttribute("x1", "2");
          line.setAttribute("x2", "28");
          line.setAttribute("y1", "9");
          line.setAttribute("y2", "9");
          line.setAttribute("stroke", c[colour.slice(1)]);
          line.setAttribute("stroke-width", "2");
          if (style) {
            line.setAttribute("stroke-dasharray", style === "dotted" ? "1 3" : "4 3");
          }
          svg.appendChild(line);
          const head = document.createElementNS(SVG, "path");
          head.setAttribute("d", "M26 5 L33 9 L26 13 Z");
          head.setAttribute("fill", c[colour.slice(1)]);
          svg.appendChild(head);
        },
        text,
      );
    }
    add(box, "h3", "badges and sizes");
    row(box, (svg) => legendShape(svg, "ellipse", c.green, c.red, 3), "red ring: open findings on the task");
    row(
      box,
      (svg) => {
        legendShape(svg, "ellipse", c.green);
        const ring = document.createElementNS(SVG, "path");
        ring.setAttribute("d", SHAPES.ellipse);
        ring.setAttribute("fill", "none");
        ring.setAttribute("stroke", c.green);
        ring.setAttribute("stroke-width", "2");
        ring.classList.add("ring");
        svg.appendChild(ring);
      },
      "ring moving outward: the task or orchestrator is working now",
    );
    add(box, "p", "A “⚠ n” in a task's label counts its open tick findings; “n asks waiting” on you counts open asks.").className =
      "note";
    add(box, "p", "A review shows “n findings (m open)”.").className = "note";
    add(box, "p", "With “size by cost”, a bigger node spent more tokens (log scale).").className = "note";
    add(box, "p", "A violet ring marks the selected node.").className = "note";
  }

  function refreshLegend() {
    const box = byId("legend");
    while (box.firstChild) {
      box.removeChild(box.firstChild);
    }
    buildLegend();
  }

  const legendToggle = byId("legend-toggle");
  legendToggle.addEventListener("click", () => {
    const box = byId("legend");
    box.hidden = !box.hidden;
    legendToggle.setAttribute("aria-expanded", String(!box.hidden));
    legendToggle.classList.toggle("on", !box.hidden);
    try {
      localStorage.setItem("legend", box.hidden ? "0" : "1");
    } catch (error) {
      status.title = String(error);
    }
  });
  refreshLegend();
  try {
    if (localStorage.getItem("legend") === "1") {
      legendToggle.click();
    }
  } catch (error) {
    status.title = String(error);
  }

  showInstant();
  loadRange().then(load);
  listen();
})();
