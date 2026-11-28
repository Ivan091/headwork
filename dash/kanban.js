"use strict";

(function () {
  const RELOAD_MS = 1000;
  const COLUMNS = {
    backlog: "Backlog",
    in_progress: "In progress",
    blocked: "Blocked",
    done: "Done",
    cancelled: "Cancelled",
  };
  const GROUPS = {
    "needs-you": "Needs you",
    working: "Working",
    "in-review": "In review",
    merged: "Merged",
    queued: "Queued",
    paused: "Paused",
    "no-task": "No task",
  };
  const byId = (id) => document.getElementById(id);
  const root = byId("board");
  const projectSelect = byId("project");
  const searchInput = byId("search");
  const status = byId("status");
  const graphButton = byId("view-graph");
  const boardButton = byId("view-board");
  const expanded = new Set();
  let board = null;
  let timer = null;
  let loading = false;
  let again = false;
  let lastLoad = 0;
  let cancelledOpen = false;

  function make(tag, className, text) {
    const element = document.createElement(tag);
    if (className) {
      element.className = className;
    }
    if (text !== undefined) {
      element.textContent = text;
    }
    return element;
  }

  function shown() {
    return document.body.dataset.view === "board";
  }

  function setView(view) {
    document.body.dataset.view = view;
    graphButton.classList.toggle("on", view === "graph");
    boardButton.classList.toggle("on", view === "board");
    try {
      localStorage.setItem("headwork-view", view);
    } catch (error) {
      status.title = String(error);
    }
    history.replaceState(null, "", view === "board" ? "#board" : location.pathname + location.search);
    window.dispatchEvent(new Event("resize"));
    if (view === "board") {
      load();
    }
  }

  function initialView() {
    if (location.hash === "#board") {
      return "board";
    }
    try {
      return localStorage.getItem("headwork-view") === "board" && location.hash === "" ? "board" : "graph";
    } catch (error) {
      return "graph";
    }
  }

  function short(tokens) {
    if (tokens >= 1e6) {
      return (tokens / 1e6).toFixed(1) + "M";
    }
    if (tokens >= 1e3) {
      return Math.round(tokens / 1e3) + "k";
    }
    return String(tokens);
  }

  function chip(text, className, title) {
    const element = make("span", "chip " + (className || ""), text);
    if (title) {
      element.title = title;
    }
    return element;
  }

  function matches(card, text) {
    if (!text) {
      return true;
    }
    const haystack = ["#" + card.item.id, card.item.text, card.task ? card.task.name : ""].join(" ").toLowerCase();
    return haystack.includes(text);
  }

  function taskChips(card, chips) {
    const task = card.task;
    const name = make("button", "chip task status-" + task.status.replace(" ", "-"), task.name + " · " + task.status);
    name.type = "button";
    name.title = "open this task";
    name.addEventListener("click", (event) => {
      event.stopPropagation();
      window.dashSelect("task:" + task.id);
    });
    chips.append(name);
    const graph = make("button", "chip link", "graph");
    graph.type = "button";
    graph.title = "show this task in the graph";
    graph.addEventListener("click", (event) => {
      event.stopPropagation();
      setView("graph");
      window.dashSelect("task:" + task.id);
    });
    chips.append(graph);
    chips.append(chip(task.model + " · " + task.effort, "model"));
    if (task.review) {
      chips.append(chip("in review L" + task.review.level + " r" + task.review.round + " · " + task.review.findings + " findings"));
    } else if (task.review_plan) {
      chips.append(chip("review " + task.review_plan + " planned"));
    }
    task.prs.forEach((url) => {
      const number = url.split("/").pop();
      const link = make("a", "chip pr", "PR #" + number);
      link.href = url;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      link.addEventListener("click", (event) => event.stopPropagation());
      chips.append(link);
    });
    if (task.parts) {
      const asking = task.asking_parts.length;
      chips.append(chip(task.parts + " parts" + (asking ? ", " + asking + " need you" : "")));
    }
    if (task.tokens) {
      chips.append(chip(short(task.tokens), "tokens", task.estimate ? "about " + task.estimate : "tokens"));
    }
    if (task.findings) {
      chips.append(chip(task.findings + " findings", "overdue"));
    }
    if (task.waiting_for) {
      chips.append(chip("waiting for " + task.waiting_for));
    }
    if (task.silent) {
      chips.append(chip("silent"));
    }
    if (task.dead_dependency) {
      chips.append(chip("waits on " + task.dead_dependency, "overdue"));
    }
  }

  function renderCard(card, today) {
    const item = card.item;
    const element = make("div", "kb-card" + (card.waiting ? " waiting" : "") + (card.held ? " held" : "") + (card.drift ? " drift" : ""));
    element.tabIndex = 0;
    const title = make("div", "kb-title");
    title.append(make("span", "kb-id", "#" + item.id + " "), make("span", "kb-text", item.text));
    element.append(title);
    const chips = make("div", "kb-chips");
    chips.append(chip(item.owner === "user" ? "you" : "agent", "owner-" + item.owner));
    if (item.kind === "epic") {
      chips.append(chip("epic"));
    }
    if (item.parent) {
      chips.append(chip("in epic #" + item.parent));
    }
    if (card.deps && card.deps.length) {
      const labels = card.deps.map((dep) => "#" + dep.id + " " + dep.status.replace("_", " "));
      const open = card.deps.some((dep) => dep.status !== "done" && dep.status !== "cancelled");
      chips.append(chip("after " + labels.join(", "), open ? "held" : "", open ? "waits for these items" : "every dependency is over"));
    }
    if (item.not_before) {
      chips.append(chip("not before " + item.not_before, item.not_before > today ? "held" : ""));
    }
    if (item.due) {
      chips.append(chip("due " + item.due, item.due < today && item.status !== "done" ? "overdue" : ""));
    }
    if (item.status === "blocked") {
      chips.append(chip("blocked: " + item.blocked_reason, "status-blocked"));
    }
    if (card.drift) {
      chips.append(chip("no live task", "overdue"));
    }
    if (item.status === "done" && item.done_on) {
      chips.append(chip("done " + item.done_on));
    }
    if (item.status === "backlog" && item.added) {
      chips.append(chip("added " + item.added));
    }
    if (card.task) {
      taskChips(card, chips);
    } else if (item.task) {
      chips.append(chip(item.task.name + " · " + item.task.state));
    }
    element.append(chips);
    const more = make("div", "kb-more");
    if (item.blocked_note) {
      more.append(make("div", "kb-note", item.blocked_note));
    }
    (item.notes || []).forEach((note) => more.append(make("div", "kb-note", note)));
    if (item.done_note) {
      more.append(make("div", "kb-note", item.done_note));
    }
    if (card.task && card.task.updated_at) {
      more.append(make("div", "kb-note", "task changed " + card.task.updated_at));
    }
    element.append(more);
    const key = String(item.id);
    element.classList.toggle("open", expanded.has(key));
    const flip = () => {
      if (expanded.has(key)) {
        expanded.delete(key);
      } else {
        expanded.add(key);
      }
      element.classList.toggle("open");
    };
    element.addEventListener("click", flip);
    element.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        flip();
      }
    });
    return element;
  }

  function laneHead(lane) {
    const head = make("h3", lane.epic ? "kb-epic" : "", lane.title);
    if (!lane.epic) {
      return head;
    }
    const epic = lane.epic;
    const box = make("div", "kb-epic-head");
    box.append(head);
    box.append(chip("epic · " + epic.item.status.replace("_", " "), "status-" + epic.item.status));
    epic.counts.forEach((entry) => box.append(chip(entry.count + " " + entry.status.replace("_", " "), "status-" + entry.status)));
    if (epic.tokens) {
      box.append(chip(short(epic.tokens), "tokens", "tokens its children's tasks spent"));
    }
    return box;
  }

  function unlinkedCard(task) {
    const element = make("div", "kb-card");
    const title = make("div", "kb-title");
    title.append(make("span", "kb-text", task.name));
    element.append(title);
    const chips = make("div", "kb-chips");
    taskChips({ task: task }, chips);
    element.append(chips);
    return element;
  }

  function strip(text) {
    const ids = new Set(board.waiting);
    const cards = [];
    for (const column of board.columns) {
      const all = column.cards.concat(...column.groups.map((group) => group.cards));
      for (const card of all) {
        if (ids.has(card.item.id) || (card.task && card.task.status === "needs-you")) {
          if (matches(card, text) && !cards.includes(card)) {
            cards.push(card);
          }
        }
      }
    }
    const box = make("section", "kb-strip");
    box.append(make("h2", "", "Needs you (" + cards.length + ")"));
    const row = make("div", "kb-strip-cards");
    cards.forEach((card) => row.append(renderCard(card, board.today)));
    if (!cards.length) {
      row.append(make("span", "note", "nothing waits on you"));
    }
    box.append(row);
    return box;
  }

  function columnBody(column, lane, text) {
    const body = make("div", "kb-col");
    const mine = (card) => card.lane === lane && matches(card, text);
    if (column.status === "in_progress") {
      for (const group of column.groups) {
        const cards = group.cards.filter(mine);
        if (!cards.length) {
          continue;
        }
        body.append(make("h4", "kb-group group-" + group.key, GROUPS[group.key] + " (" + cards.length + ")"));
        cards.forEach((card) => body.append(renderCard(card, board.today)));
      }
      return body;
    }
    if (column.status === "cancelled" && !cancelledOpen) {
      return body;
    }
    column.cards.filter(mine).forEach((card) => body.append(renderCard(card, board.today)));
    return body;
  }

  function render() {
    if (!board) {
      return;
    }
    const text = searchInput.value.trim().toLowerCase();
    root.replaceChildren();
    root.append(strip(text));
    const heads = make("div", "kb-grid kb-heads");
    for (const column of board.columns) {
      const head = make("div", "kb-head status-" + column.status);
      head.append(make("span", "", COLUMNS[column.status] + " "), make("span", "kb-count", String(column.count)));
      if (column.status === "cancelled") {
        head.classList.add("clickable");
        head.title = "show or hide the cancelled items";
        head.addEventListener("click", () => {
          cancelledOpen = !cancelledOpen;
          render();
        });
      }
      if (column.more) {
        head.append(make("span", "kb-more-done", " · " + column.more + " older not shown"));
      }
      heads.append(head);
    }
    root.append(heads);
    for (const lane of board.lanes) {
      const section = make("section", "kb-lane");
      section.append(laneHead(lane));
      const grid = make("div", "kb-grid");
      board.columns.forEach((column) => grid.append(columnBody(column, lane.key, text)));
      section.append(grid);
      root.append(section);
    }
    const loose = board.unlinked.filter((task) => !text || task.name.toLowerCase().includes(text));
    if (loose.length) {
      const section = make("section", "kb-lane");
      section.append(make("h3", "", "No item (" + loose.length + ")"));
      const row = make("div", "kb-strip-cards");
      loose.forEach((task) => row.append(unlinkedCard(task)));
      section.append(row);
      root.append(section);
    }
  }

  function url() {
    const project = projectSelect.value;
    return "/api/kanban" + (project ? "?project=" + encodeURIComponent(project) : "");
  }

  async function load() {
    if (!shown()) {
      return;
    }
    if (loading) {
      again = true;
      return;
    }
    loading = true;
    try {
      const response = await fetch(url(), { cache: "no-store" });
      const body = await response.json();
      if (!response.ok) {
        throw new Error(body.error || response.statusText);
      }
      board = body;
      lastLoad = Date.now();
      render();
    } catch (error) {
      root.replaceChildren(make("p", "note", "cannot read the board: " + error.message));
    } finally {
      loading = false;
      if (again) {
        again = false;
        schedule();
      }
    }
  }

  function schedule() {
    if (!shown() || timer) {
      return;
    }
    const wait = Math.max(0, RELOAD_MS - (Date.now() - lastLoad));
    timer = setTimeout(() => {
      timer = null;
      load();
    }, wait);
  }

  document.addEventListener("dash-change", schedule);
  projectSelect.addEventListener("change", load);
  searchInput.addEventListener("input", render);
  graphButton.addEventListener("click", () => setView("graph"));
  boardButton.addEventListener("click", () => setView("board"));
  setView(initialView());
})();
