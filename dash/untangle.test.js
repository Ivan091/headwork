const assert = require("node:assert");
const u = require("./untangle.js");
const D = u.DEFAULTS;

const xTangle = () => ({
  ids: ["a", "b", "c", "d"],
  edges: [["a", "d"], ["b", "c"]],
  pos: { a: { x: 0, y: 0 }, b: { x: 100, y: 0 }, c: { x: 0, y: 100 }, d: { x: 100, y: 100 } },
});

{
  const { ids, edges, pos } = xTangle();
  assert.strictEqual(u.countAll(edges, pos), 1);
  const before = JSON.stringify(pos);
  const out = u.untangle(ids, edges, pos, D);
  assert.strictEqual(out.crossings, 0);
  assert.strictEqual(JSON.stringify(pos), before);
}

{
  let seed = 11;
  const rnd = () => (seed = (seed * 16807) % 2147483647) / 2147483647;
  const ids = [];
  const pos = {};
  const edges = [];
  for (let i = 0; i < 30; i++) {
    ids.push("n" + i);
    pos["n" + i] = { x: rnd() * 500, y: rnd() * 500 };
    if (i) edges.push(["n" + Math.floor(rnd() * i), "n" + i]);
  }
  const out = u.untangle(ids, edges, pos, D);
  assert.ok(out.crossings <= u.countAll(edges, pos));
  edges.push(["n1", "n1"]);
  assert.ok(u.untangle(ids, edges, pos, D).crossings <= u.countAll(edges, pos));
}

{
  const { ids, edges, pos } = xTangle();
  pos.a.x = NaN;
  assert.strictEqual(u.untangle(ids, edges, pos, D).crossings, -1);
  const missing = xTangle();
  delete missing.pos.b;
  assert.strictEqual(u.untangle(missing.ids, missing.edges, missing.pos, D).crossings, -1);
  const capped = xTangle();
  assert.strictEqual(u.untangle(capped.ids, capped.edges, capped.pos, { ...D, maxNodes: 2 }).crossings, -1);
}

{
  const { ids, edges, pos } = xTangle();
  const out = u.untangle(ids, [...edges, ["a", "x"]].filter((e) => e[1] !== "x"), pos, D);
  const spots = new Set(ids.map((id) => out.positions[id].x + "," + out.positions[id].y));
  assert.strictEqual(spots.size, 4);
}
{
  const ids = ["a", "b", "c", "d", "e"];
  const pos = { a: { x: 0, y: 0 }, b: { x: 100, y: 100 }, c: { x: 0, y: 100 }, d: { x: 100, y: 0 }, e: { x: 50, y: 50 } };
  const edges = [["a", "b"], ["c", "d"], ["a", "a"], ["a", "ghost"], ["ghost", "e"]];
  const out = u.untangle(ids, edges, pos, D);
  assert.strictEqual(out.crossings, 0);
  for (const x of ids) {
    for (const y of ids) {
      if (x < y) {
        const p = out.positions[x];
        const q = out.positions[y];
        assert.ok(Math.hypot(p.x - q.x, p.y - q.y) >= D.gap, "gap holds between " + x + " and " + y);
      }
    }
  }
}
console.log("untangle tests passed");
