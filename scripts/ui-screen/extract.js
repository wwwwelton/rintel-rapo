// Walks the visible DOM in paint order and emits rect/text/circle nodes in CSS px relative to the page.
(() => {
  const nodes = [];
  const add = (n) => nodes.push(n);
  const r2 = (v) => Math.round(v * 100) / 100;
  const color = (c) => (c && c !== 'rgba(0, 0, 0, 0)' && c !== 'transparent') ? c : null;
  const sx = scrollX, sy = scrollY;
  const visible = (el, cs) => cs.display !== 'none' && cs.visibility !== 'hidden' && !el.hidden && el.getClientRects().length;
  function box(el, cs) {
    const b = el.getBoundingClientRect(); if (b.width < 1 || b.height < 1) return;
    const bg = color(cs.backgroundColor);
    const bw = parseFloat(cs.borderTopWidth) || 0; const bc = bw > 0 && cs.borderTopStyle !== 'none' ? color(cs.borderTopColor) : null;
    if (!bg && !bc) return;
    add({ type: 'rect', attributes: { x: r2(b.left + sx), y: r2(b.top + sy), width: r2(b.width), height: r2(b.height), rx: r2(parseFloat(cs.borderTopLeftRadius) || 0), fill: bg || 'none', stroke: bc || 'none', 'stroke-width': r2(bc ? bw : 0), opacity: 1 } });
  }
  function toggle(el, cs) {
    const b = el.getBoundingClientRect(); const on = el.checked;
    add({ type: 'rect', attributes: { x: r2(b.left + sx), y: r2(b.top + sy), width: r2(b.width), height: r2(b.height), rx: r2(b.height / 2), fill: on ? 'rgba(101, 229, 209, 0.24)' : 'rgb(37, 46, 40)', stroke: on ? 'rgb(101, 229, 209)' : 'rgb(88, 103, 94)', 'stroke-width': 1, opacity: el.disabled ? 0.48 : 1 } });
    add({ type: 'circle', attributes: { cx: r2(b.left + sx + (on ? b.width - 10 : 10)), cy: r2(b.top + sy + b.height / 2), r: 7, fill: on ? '#65e5d1' : '#b1beb5', opacity: el.disabled ? 0.48 : 1 } });
  }
  const transform = (t, m) => m === 'uppercase' ? t.toUpperCase() : m === 'lowercase' ? t.toLowerCase() : m === 'capitalize' ? t.replace(/\b\w/g, (c) => c.toUpperCase()) : t;
  function texts(el, cs) {
    for (const node of el.childNodes) {
      if (node.nodeType !== 3 || !node.textContent.trim()) continue;
      const range = document.createRange(); range.selectNodeContents(node);
      // One scene text per rendered line: group client rects by top.
      const text = node.textContent; let line = ''; let lineTop = null; let lineLeft = 0; let lineBottom = 0;
      const flush = () => { if (line.trim()) add({ type: 'text', attributes: { x: r2(lineLeft + sx), y: r2(lineBottom + sy - (parseFloat(cs.fontSize) * 0.22)), fill: cs.color, 'font-family': cs.fontFamily, 'font-size': r2(parseFloat(cs.fontSize)), 'font-weight': cs.fontWeight, opacity: parseFloat(cs.opacity) || 1 }, text: transform(line.trim().replace(/\s+/g, ' '), cs.textTransform) }); line = ''; };
      for (let i = 0; i < text.length; i++) {
        const r = document.createRange(); r.setStart(node, i); r.setEnd(node, i + 1);
        const rects = r.getClientRects(); if (!rects.length) { line += text[i]; continue; }
        const q = rects[0]; if (q.width === 0 && /\s/.test(text[i])) { line += ' '; continue; }
        if (lineTop === null || Math.abs(q.top - lineTop) > 3) { flush(); lineTop = q.top; lineLeft = q.left; lineBottom = q.bottom; }
        line += text[i]; lineBottom = Math.max(lineBottom, q.bottom);
      }
      flush(); lineTop = null;
    }
  }
  function walk(el) {
    const cs = getComputedStyle(el); if (!visible(el, cs)) return;
    if (el.tagName === 'INPUT' && el.type === 'checkbox') { toggle(el, cs); return; }
    if (el.tagName === 'INPUT' || el.tagName === 'SELECT' || el.tagName === 'TEXTAREA') {
      box(el, cs); const b = el.getBoundingClientRect(); const v = el.tagName === 'SELECT' ? el.selectedOptions[0]?.textContent : (el.value || el.placeholder);
      if (v) add({ type: 'text', attributes: { x: r2(b.left + sx + 13), y: r2(b.top + sy + b.height / 2 + 5), fill: el.value ? cs.color : '#87958c', 'font-family': cs.fontFamily, 'font-size': r2(parseFloat(cs.fontSize)), 'font-weight': cs.fontWeight, opacity: 1 }, text: String(v) }); return;
    }
    box(el, cs); texts(el, cs);
    for (const child of el.children) { if (el.tagName === 'DETAILS' && !el.open && child.tagName !== 'SUMMARY') continue; walk(child); }
  }
  walk(document.body);
  return { nodes, width: Math.ceil(document.documentElement.scrollWidth), height: Math.ceil(document.documentElement.scrollHeight) };
})()
