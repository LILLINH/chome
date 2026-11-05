/**
 * @file json_viewer.js
 * @description Trình xem JSON tích hợp của Chỏme. Chạy khi trang tải xong; chỉ hoạt động
 * nếu tài liệu là JSON (Content-Type …/json, hoặc text/plain chứa JSON hợp lệ). Thay khối
 * chữ thô bằng cây thu gọn/mở rộng được, có tô màu, tìm kiếm, sao chép, đường dẫn khóa.
 * Dựng lười (chỉ tạo nút khi mở ra) để xem được cả file vài chục MB.
 */
(() => {
  'use strict';
  if (window.top !== window || window.__chomeJson) return;
  const type = (document.contentType || '').toLowerCase();
  const jsonType = /(^|[/+])json$/.test(type);
  if (!jsonType && type !== 'text/plain') return;
  const body = document.body;
  if (!body || body.children.length > 1) return;
  const pre = body.querySelector('pre');
  const raw = (pre || body).textContent;
  if (!raw || raw.length > 50 * 1024 * 1024) return;
  const trimmed = raw.trim();
  if (!jsonType && !/^[\[{]/.test(trimmed)) return;
  let data;
  try { data = JSON.parse(trimmed); } catch (_) { return; }
  window.__chomeJson = true;

  const CHUNK = 100;
  const OPEN_DEPTH = 2;


  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  };

  const pathOf = (parent, key) => {
    if (typeof key === 'number') return parent + '[' + key + ']';
    return /^[A-Za-z_$][\w$]*$/.test(key) ? parent + '.' + key : parent + '[' + JSON.stringify(key) + ']';
  };

  const countOf = (v) => (Array.isArray(v) ? v.length : Object.keys(v).length);

  // Giá trị đơn (không phải object/array).
  const scalar = (v) => {
    if (v === null) return el('span', 'nl', 'null');
    switch (typeof v) {
      case 'string': {
        const text = JSON.stringify(v);
        if (/^https?:\/\/\S+$/.test(v)) {
          const a = el('a', 's', text); a.href = v; a.target = '_blank'; a.rel = 'noreferrer';
          return a;
        }
        return el('span', 's', text);
      }
      case 'number': return el('span', 'num', String(v));
      case 'boolean': return el('span', 'b', String(v));
      default: return el('span', '', String(v));
    }
  };

  // Một dòng: [khóa:] giá trị — object/array dựng con khi mở (lười).
  const node = (key, value, path, depth, last) => {
    const row = el('div', depth === 0 ? 'n root' : 'n');
    row.dataset.path = path;
    if (key !== undefined) {
      const k = el('span', 'k', typeof key === 'number' ? String(key) : JSON.stringify(key));
      row.append(k, el('span', 'p', ': '));
    }
    const comma = last ? '' : ',';
    if (value === null || typeof value !== 'object') {
      row.append(scalar(value), el('span', 'p', comma));
      return row;
    }
    const isArr = Array.isArray(value);
    const n = countOf(value);
    const [open, close] = isArr ? ['[', ']'] : ['{', '}'];
    if (n === 0) {
      row.append(el('span', 'p', open + close + comma));
      return row;
    }
    const toggle = el('span', 't');
    row.prepend(toggle);
    row.append(el('span', 'p', open));
    const sum = el('span', 'sum', ' … ' + n + (isArr ? ' mục ' : ' khóa ') + close + comma);
    const kids = el('div', 'kids');
    const end = el('div', 'end p', close + comma);
    row.append(sum, kids, end);
    row._value = value;
    row._depth = depth;
    row._built = false;
    if (depth >= OPEN_DEPTH) row.classList.add('closed');
    else build(row);
    return row;
  };

  // Dựng con của một object/array (từng đợt CHUNK mục với mảng/obj lớn).
  const build = (row, from = 0) => {
    const value = row._value;
    const kids = row.querySelector(':scope > .kids');
    const keys = Array.isArray(value) ? null : Object.keys(value);
    const n = keys ? keys.length : value.length;
    const to = Math.min(n, from + CHUNK);
    const frag = document.createDocumentFragment();
    for (let i = from; i < to; i++) {
      const key = keys ? keys[i] : i;
      frag.append(node(key, value[key], pathOf(row.dataset.path, key), row._depth + 1, i === n - 1));
    }
    const oldMore = kids.querySelector(':scope > .more');
    if (oldMore) oldMore.remove();
    kids.append(frag);
    if (to < n) {
      const more = el('div', 'more', '▸ Hiện thêm ' + Math.min(CHUNK, n - to) + ' / còn ' + (n - to) + ' mục…');
      more.onclick = () => build(row, to);
      kids.append(more);
    }
    row._built = true;
  };

  const setOpen = (row, open) => {
    if (open && !row._built) build(row);
    row.classList.toggle('closed', !open);
  };

  // ---------- Giao diện ----------
  document.title = document.title || location.pathname.split('/').pop() || 'JSON';
  // CSS do trình duyệt chèn (json_viewer.css) để không bị CSP của trang chặn.
  document.documentElement.classList.add('chome-json');
  const bar = el('div', 'bar');
  const bPretty = el('button', 'on', 'Đẹp');
  const bRaw = el('button', '', 'Thô');
  const bExpand = el('button', '', 'Mở hết');
  const bCollapse = el('button', '', 'Thu gọn hết');
  const bCopy = el('button', '', 'Sao chép');
  const search = el('input'); search.placeholder = 'Tìm khóa hoặc giá trị…'; search.type = 'search';
  const size = new Blob([raw]).size;
  const sizeText = size > 1048576 ? (size / 1048576).toFixed(1) + ' MB' : (size / 1024).toFixed(1) + ' KB';
  const top = data !== null && typeof data === 'object' ? countOf(data) + (Array.isArray(data) ? ' mục' : ' khóa') + ' · ' : '';
  const info = el('span', 'info', 'JSON · ' + top + sizeText);
  bar.append(bPretty, bRaw, bExpand, bCollapse, bCopy, search, info);
  const pathBar = el('div', 'path', 'Bấm vào một khóa để xem đường dẫn');
  const tree = el('div'); tree.id = 'tree';
  const rawView = el('pre'); rawView.id = 'raw'; rawView.textContent = JSON.stringify(data, null, 2);
  tree.append(node(undefined, data, '$', 0, true));

  body.textContent = '';
  bar.append(pathBar); // luôn thấy đường dẫn khóa đang chọn, kể cả khi cuộn
  body.append(bar, tree, rawView);

  bPretty.onclick = () => { tree.style.display = ''; rawView.style.display = 'none'; bPretty.classList.add('on'); bRaw.classList.remove('on'); };
  bRaw.onclick = () => { tree.style.display = 'none'; rawView.style.display = 'block'; bRaw.classList.add('on'); bPretty.classList.remove('on'); };

  const allRows = () => tree.querySelectorAll('.n');
  bExpand.onclick = () => {
    // Mở dần từng tầng (giới hạn để không treo với file khổng lồ).
    let guard = 0;
    let changed = true;
    while (changed && guard++ < 50) {
      changed = false;
      for (const r of tree.querySelectorAll('.n.closed')) { setOpen(r, true); changed = true; }
      if (tree.querySelectorAll('.n').length > 20000) break;
    }
  };
  bCollapse.onclick = () => {
    for (const r of allRows()) if (r._value && r !== tree.firstChild) r.classList.add('closed');
  };

  const flash = (btn, text) => { const old = btn.textContent; btn.textContent = text; setTimeout(() => (btn.textContent = old), 1200); };
  const copy = (text, btn) => {
    (navigator.clipboard ? navigator.clipboard.writeText(text) : Promise.reject())
      .then(() => btn && flash(btn, 'Đã chép ✓'))
      .catch(() => {
        const t = el('textarea'); t.value = text; body.append(t); t.select();
        document.execCommand('copy'); t.remove(); if (btn) flash(btn, 'Đã chép ✓');
      });
  };
  bCopy.onclick = () => copy(JSON.stringify(data, null, 2), bCopy);

  tree.addEventListener('click', (e) => {
    const t = e.target;
    const row = t.closest('.n');
    if (!row) return;
    if (t.classList.contains('t') || t.classList.contains('sum')) {
      setOpen(row, row.classList.contains('closed'));
      return;
    }
    if (t.classList.contains('k')) {
      for (const s of tree.querySelectorAll('.selected')) s.classList.remove('selected');
      row.classList.add('selected');
      pathBar.textContent = 'Đường dẫn: ';
      const b = el('b', '', row.dataset.path); b.title = 'Bấm để sao chép';
      b.onclick = () => copy(row.dataset.path, null);
      pathBar.append(b);
    }
  });

  // Tìm kiếm: mở các nhánh chứa kết quả và tô sáng (tối đa 500 kết quả).
  let timer = 0;
  search.addEventListener('input', () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      for (const m of tree.querySelectorAll('mark')) m.replaceWith(m.textContent);
      const q = search.value.trim().toLowerCase();
      if (!q) { info.textContent = 'JSON · ' + top + sizeText; return; }
      const hits = [];
      const walk = (v, path) => {
        if (hits.length >= 500) return;
        if (v !== null && typeof v === 'object') {
          for (const k of Array.isArray(v) ? v.keys() : Object.keys(v)) {
            const p = pathOf(path, k);
            if (String(k).toLowerCase().includes(q)) hits.push(p);
            walk(v[k], p);
          }
        } else if (String(v).toLowerCase().includes(q)) hits.push(path);
      };
      walk(data, '$');
      info.textContent = hits.length >= 500 ? '500+ kết quả' : hits.length + ' kết quả';
      // Mở đường tới từng kết quả.
      for (const p of hits.slice(0, 200)) reveal(p);
      const re = new RegExp(q.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'ig');
      for (const span of tree.querySelectorAll('.k, .s, .num, .b, .nl')) {
        if (span.textContent.toLowerCase().includes(q) && !span.querySelector('mark')) {
          span.innerHTML = span.textContent.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]))
            .replace(re, (m) => '<mark>' + m + '</mark>');
        }
      }
      const first = tree.querySelector('mark');
      if (first) first.scrollIntoView({ block: 'center' });
    }, 200);
  });

  // Mở dần các nhánh trên đường dẫn path (kể cả nạp thêm đợt CHUNK nếu cần).
  const reveal = (path) => {
    let row = tree.firstChild;
    while (row && row.dataset.path !== path) {
      if (row._value === undefined) return;
      setOpen(row, true);
      let next = null;
      for (let guard = 0; guard < 1000 && !next; guard++) {
        next = [...row.querySelectorAll(':scope > .kids > .n')].find((r) => path === r.dataset.path || path.startsWith(r.dataset.path + '.') || path.startsWith(r.dataset.path + '['));
        if (!next) {
          const more = row.querySelector(':scope > .kids > .more');
          if (!more) break;
          more.click();
        }
      }
      row = next;
    }
  };
})();
