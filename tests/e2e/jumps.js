// The jump detector, injected in the page by dashboard.js: watches every running CSS animation and transition frame by frame,
// and the layout shifts. window.__mon.start(); ... act ...; await window.__mon.stop() -> faults: "double" (two see-through
// text layers over each other), "snap" (the last animated place is not the resting place), "shift" (the layout moved more
// than 12 px in one frame: an eased height moves less per frame; the front page stars, `.fx`, are left out).
(() => {
  const name = (el) => {
    if (!el || !el.tagName) return String(el);
    let s = el.tagName.toLowerCase();
    if (el.id) s += '#' + el.id;
    if (el.classList && el.classList.length) s += '.' + [...el.classList].slice(0, 3).join('.');
    const p = el.parentElement;
    return (p && p.classList && p.classList.length ? p.tagName.toLowerCase() + '.' + [...p.classList].slice(0, 2).join('.') + ' > ' : '') + s;
  };
  const hasText = (el) => (el.innerText || '').trim().length > 3;
  const opac = (el) => { let o = 1; for (let e = el; e && e.nodeType === 1; e = e.parentElement) o *= +getComputedStyle(e).opacity; return o; };
  let on = false, frames = [], shifts = [], seen = new Map(), path = new Map();
  const po = new PerformanceObserver((l) => { if (!on) return; for (const e of l.getEntries()) for (const s of e.sources || []) {
    const a = s.previousRect, b = s.currentRect; if (!a.width || !a.height || !b.width || !b.height) continue; // comes from or goes to nothing: an ease that opens or closes it const d = Math.max(Math.abs(a.x - b.x), Math.abs(a.y - b.y));
    const nd = s.node && (s.node.nodeType === 1 ? s.node : s.node.parentElement); if (nd && d > 12 && opac(nd) > 0.05 && !nd.closest('.fx')) shifts.push({ el: name(s.node.nodeType === 1 ? s.node : s.node.parentElement), d: Math.round(d), dx: Math.round(b.x - a.x), dy: Math.round(b.y - a.y), dh: Math.round(b.height - a.height) }); } });
  try { po.observe({ type: 'layout-shift', buffered: false }); } catch (e) {}
  const tick = () => {
    if (!on) return;
    const f = [];
    for (const a of document.getAnimations()) {
      const el = a.effect && a.effect.target; if (!el || el.nodeType !== 1) continue;
      const r = el.getBoundingClientRect();
      if (r.width < 2 || r.height < 2) continue;
      const o = opac(el);
      f.push({ el, r: { x: r.x, y: r.y, w: r.width, h: r.height }, o, p: a.transitionProperty || a.animationName || '' });
      seen.set(el, true);
    }
    // Every element seen moving is followed in every frame, to tell an end that snaps from one that keeps easing (a parent's ease).
    for (const el of seen.keys()) { if (!path.has(el)) path.set(el, []); const r = el.getBoundingClientRect(); path.get(el)[frames.length] = { x: r.x, y: r.y, w: r.width, h: r.height }; }
    frames.push(f);
    requestAnimationFrame(tick);
  };
  window.__mon = {
    start() { on = true; frames = []; shifts = []; seen = new Map(); path = new Map(); requestAnimationFrame(tick); },
    async stop(settle = 700) {
      await new Promise((r) => setTimeout(r, settle));
      on = false;
      const faults = [];
      // 1. Two text pieces readable through each other: both partly see-through and overlapping in one frame.
      for (const f of frames) {
        const mid = f.filter((x) => x.o > 0.2 && x.o < 0.8 && hasText(x.el));
        for (let i = 0; i < mid.length; i++) for (let j = i + 1; j < mid.length; j++) {
          const a = mid[i], b = mid[j];
          if (a.el.contains(b.el) || b.el.contains(a.el)) continue;
          const ox = Math.min(a.r.x + a.r.w, b.r.x + b.r.w) - Math.max(a.r.x, b.r.x), oy = Math.min(a.r.y + a.r.h, b.r.y + b.r.h) - Math.max(a.r.y, b.r.y);
          if (ox > 0 && oy > 0 && ox * oy > 0.3 * Math.min(a.r.w * a.r.h, b.r.w * b.r.h)) faults.push({ kind: 'double', el: name(a.el) + ' + ' + name(b.el), o: [a.o.toFixed(2), b.o.toFixed(2)] });
        }
      }
      // 2. Snap at the end: the last animated place differs from the resting place, and the element got there in one frame.
      const last = new Map();
      frames.forEach((f, i) => { for (const x of f) last.set(x.el, [x.r, i]); });
      const gap = (a, b) => Math.max(Math.abs(a.x - b.x), Math.abs(a.y - b.y), Math.abs(a.w - b.w), Math.abs(a.h - b.h));
      for (const [el, [r, i]] of last) {
        if (!el.isConnected || opac(el) < 0.05) continue;
        if (document.getAnimations().some((a) => a.effect && a.effect.target === el)) continue; // still running (a loop)
        const b = el.getBoundingClientRect(), n = { x: b.x, y: b.y, w: b.width, h: b.height };
        if (!b.width && !b.height) continue; // not drawn any more (a hidden part)
        const d = gap(n, r), next = (path.get(el) || [])[i + 1];
        if (d > 1.5 && !(next && gap(next, n) > 0.5 && gap(next, r) < d / 2)) faults.push({ kind: 'snap', el: name(el), d: +d.toFixed(1) });
      }
      // 3. Layout shifts.
      for (const s of shifts) faults.push({ kind: 'shift', ...s });
      const uniq = new Map(); for (const x of faults) { const k = x.kind + x.el; if (!uniq.has(k)) uniq.set(k, x); }
      return { frames: frames.length, animated: [...seen.keys()].map(name).slice(0, 12), faults: [...uniq.values()].slice(0, 25) };
    },
  };
})();
