/**
 * Crossing reduction for a laid-out graph: a deterministic local search that moves nodes to cut straight-edge crossings.
 */
(function (root) {
  function cross(a, b, c, d) {
    const o = (p, q, r) => (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
    const d1 = o(a, b, c);
    const d2 = o(a, b, d);
    const d3 = o(c, d, a);
    const d4 = o(c, d, b);
    return d1 * d2 < 0 && d3 * d4 < 0;
  }

  function countAll(edges, pos) {
    let total = 0;
    for (let i = 0; i < edges.length; i++) {
      for (let j = i + 1; j < edges.length; j++) {
        total += pair(edges[i], edges[j], pos);
      }
    }
    return total;
  }

  function pair(e, f, pos) {
    if (e[0] === f[0] || e[0] === f[1] || e[1] === f[0] || e[1] === f[1]) {
      return 0;
    }
    return cross(pos[e[0]], pos[e[1]], pos[f[0]], pos[f[1]]) ? 1 : 0;
  }

  function around(node, edges, pos) {
    let total = 0;
    for (const e of edges.mine[node]) {
      for (const f of edges.all) {
        if (e !== f) {
          total += pair(e, f, pos);
        }
      }
    }
    return total;
  }

  function untangle(ids, edgeList, positions, options) {
    const gap = (options && options.gap) || 40;
    const rounds = (options && options.rounds) || 12;
    const pos = {};
    for (const id of ids) {
      pos[id] = { x: positions[id].x, y: positions[id].y };
    }
    const all = edgeList.filter((e) => e[0] !== e[1] && pos[e[0]] && pos[e[1]]);
    const mine = {};
    for (const id of ids) {
      mine[id] = [];
    }
    for (const e of all) {
      mine[e[0]].push(e);
      mine[e[1]].push(e);
    }
    const edges = { all, mine };
    const clear = (id, p) => ids.every((other) => other === id || Math.hypot(pos[other].x - p.x, pos[other].y - p.y) >= gap);
    let best = countAll(all, pos);
    for (let round = 0; round < rounds && best > 0; round++) {
      let improved = false;
      for (const id of ids) {
        if (mine[id].length === 0) {
          continue;
        }
        const here = pos[id];
        const before = around(id, edges, pos);
        if (before === 0) {
          continue;
        }
        const candidates = [];
        for (const other of ids) {
          if (other !== id) {
            candidates.push({ swap: other, p: pos[other] });
          }
        }
        for (const e of mine[id]) {
          const nb = pos[e[0] === id ? e[1] : e[0]];
          candidates.push({ p: { x: (here.x + nb.x) / 2, y: (here.y + nb.y) / 2 } });
          candidates.push({ p: { x: nb.x + gap * 1.5, y: nb.y } });
          candidates.push({ p: { x: nb.x - gap * 1.5, y: nb.y } });
          candidates.push({ p: { x: nb.x, y: nb.y + gap * 1.5 } });
          candidates.push({ p: { x: nb.x, y: nb.y - gap * 1.5 } });
        }
        let pick = null;
        for (const c of candidates) {
          const origin = { x: here.x, y: here.y };
          let score;
          if (c.swap) {
            const other = pos[c.swap];
            const keep = { x: other.x, y: other.y };
            const was = around(id, edges, pos) + around(c.swap, edges, pos) - sharedBetween(id, c.swap, edges, pos);
            pos[id] = keep;
            pos[c.swap] = origin;
            score = around(id, edges, pos) + around(c.swap, edges, pos) - sharedBetween(id, c.swap, edges, pos);
            pos[id] = origin;
            pos[c.swap] = keep;
            if (score < was && was - score > (pick ? pick.gain : 0)) {
              pick = c;
              pick.gain = was - score;
            }
          } else {
            if (!clear(id, c.p)) {
              continue;
            }
            pos[id] = c.p;
            score = around(id, edges, pos);
            pos[id] = origin;
            if (score < before && before - score > (pick ? pick.gain : 0)) {
              pick = c;
              pick.gain = before - score;
            }
          }
        }
        if (pick) {
          if (pick.swap) {
            const other = pos[pick.swap];
            pos[pick.swap] = pos[id];
            pos[id] = other;
          } else {
            pos[id] = pick.p;
          }
          improved = true;
        }
      }
      best = countAll(all, pos);
      if (!improved) {
        break;
      }
    }
    return { positions: pos, crossings: best };
  }

  function sharedBetween(a, b, edges, pos) {
    let total = 0;
    for (const e of edges.mine[a]) {
      for (const f of edges.mine[b]) {
        if (e !== f) {
          total += pair(e, f, pos);
        }
      }
    }
    return total;
  }

  const api = { untangle, countAll };
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  } else {
    root.HeadworkUntangle = api;
  }
})(typeof window !== "undefined" ? window : globalThis);
