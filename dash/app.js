"use strict";

(function () {
  const REFRESH_MS = 5000;
  const toggles = {
    subagent: "show-subagents",
    helper: "show-subagents",
    job: "show-jobs",
    pr: "show-prs",
    review: "show-reviews",
    "review-node": "show-reviews",
    ask: "show-asks",
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

  let selected = null;
  let focused = null;
  let loading = false;
  let firstLayout = true;

  function colours() {
    const css = getComputedStyle(document.documentElement);
    const read = (name) => css.getPropertyValue(name).trim();
    return {
      bg: read("--bg"),
      text: read("--text"),
      muted: read("--muted"),
      line: read("--line"),
      box: read("--box"),
      boxLine: read("--box-line"),
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
          width: 22,
          height: 22,
          "border-width": 0,
        },
      },
      { selector: "node[kind = 'user']", style: { shape: "star", width: 34, height: 34 } },
      { selector: "node[kind = 'orchestrator']", style: { shape: "hexagon", width: 40, height: 40, "font-size": 12 } },
      { selector: "node[kind = 'lead'], node[kind = 'worker'], node[kind = 'part']", style: { shape: "ellipse", width: 28, height: 28 } },
      { selector: "node[kind = 'subagent'], node[kind = 'helper']", style: { width: 12, height: 12, "font-size": 8 } },
      { selector: "node[kind = 'job']", style: { shape: "round-rectangle", width: 16, height: 12 } },
      { selector: "node[kind = 'pr']", style: { shape: "tag", width: 16, height: 14 } },
      { selector: "node[kind = 'review']", style: { shape: "diamond", width: 20, height: 20 } },
      { selector: "node[kind = 'review-node']", style: { shape: "diamond", width: 10, height: 10, "font-size": 8 } },
      { selector: "node[kind = 'ask']", style: { shape: "round-triangle", width: 20, height: 20 } },
      { selector: "node[kind = 'external']", style: { shape: "round-octagon", "background-color": c.grey } },
      {
        selector: "node[kind = 'task']",
        style: {
          shape: "round-rectangle",
          "background-color": c.box,
          "background-opacity": 1,
          "border-width": 2,
          "border-color": (node) => c[node.data("tone")] || c.boxLine,
          "text-valign": "top",
          "text-halign": "center",
          "text-margin-y": -4,
          "font-size": 11,
          "font-weight": "bold",
          padding: 12,
        },
      },
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
      { selector: "edge[kind = 'part-of']", style: { display: "none" } },
      { selector: ".hidden", style: { display: "none" } },
      { selector: ".faded", style: { opacity: 0.12 } },
      { selector: "node:selected", style: { "border-width": 3, "border-color": c.violet } },
    ];
  }

  cytoscape.use(cytoscapeFcose);
  const cy = cytoscape({
    container: byId("graph"),
    style: style(),
    wheelSensitivity: 0.3,
    minZoom: 0.05,
    maxZoom: 4,
  });

  function graphUrl() {
    return "/api/graph?scope=" + encodeURIComponent(scopeSelect.value);
  }

  function projectsOf(nodes) {
    const parents = new Map(nodes.map((node) => [node.id, node.parent]));
    const projects = new Map();
    for (const node of nodes) {
      if (node.kind === "task" && node.details && node.details.project) {
        projects.set(node.id, node.details.project);
      }
    }
    const find = (id) => {
      let at = id;
      for (let hops = 0; at && hops < 6; hops += 1) {
        if (projects.has(at)) {
          return projects.get(at);
        }
        at = parents.get(at);
      }
      return "";
    };
    return new Map(nodes.map((node) => [node.id, find(node.id)]));
  }

  function shownLabel(node) {
    if (node.kind === "task" && node.state) {
      return node.label + " · " + node.state;
    }
    return node.label;
  }

  function apply(graph) {
    const projects = projectsOf(graph.nodes);
    const known = new Set(graph.nodes.map((node) => node.id));
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
          project: projects.get(node.id) || "",
          raw: node,
        };
        const existing = cy.getElementById(node.id);
        if (existing.nonempty() && existing.data("parentId") === (node.parent || "")) {
          existing.data(data);
        } else {
          if (existing.nonempty()) {
            existing.remove();
          }
          cy.add({ group: "nodes", data: Object.assign({ parent: node.parent || undefined, parentId: node.parent || "" }, data) });
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
    status.classList.remove("error");
    status.textContent = "updated " + new Date().toLocaleTimeString() + " · " + graph.nodes.length + " nodes";
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
        node.toggleClass("hidden", Boolean(offByKind || offByProject));
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

  function layout() {
    const visible = cy.elements().not(".hidden");
    const options =
      layoutSelect.value === "breadthfirst"
        ? { name: "breadthfirst", directed: true, roots: "#orchestrator", spacingFactor: 1.2, animate: false }
        : {
            name: "fcose",
            quality: "default",
            randomize: firstLayout,
            animate: !firstLayout,
            animationDuration: 400,
            nodeDimensionsIncludeLabels: true,
            nodeRepulsion: 6000,
            idealEdgeLength: 70,
            packComponents: true,
          };
    visible.layout(options).run();
    if (firstLayout) {
      cy.fit(visible, 30);
      firstLayout = false;
    }
  }

  async function load() {
    if (loading) {
      return;
    }
    loading = true;
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
    }
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
  for (const id of new Set(Object.values(toggles))) {
    byId(id).addEventListener("change", () => {
      filter();
      layout();
    });
  }
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => cy.style(style()));
  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      if (focused) {
        unfocus();
      } else {
        closePanel();
      }
    }
  });

  load();
  setInterval(load, REFRESH_MS);
})();
