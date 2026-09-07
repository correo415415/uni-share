/* uni-share web GUI — vanilla JS over the local JSON API (see src/gui.rs). */
'use strict';
const $ = (s, r = document) => r.querySelector(s), $$ = (s, r = document) => [...r.querySelectorAll(s)];
const h = (s) => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const fmtB = (n) => { n = Number(n) || 0; const u = ['B', 'KiB', 'MiB', 'GiB', 'TiB']; let i = 0; while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; } return (i ? n.toFixed(n < 10 ? 2 : 1) : n) + ' ' + u[i]; };
const fmtRate = (n) => fmtB(n) + '/s';
const fmtEta = (s) => { if (s == null) return '—'; s = Math.round(s); if (s < 60) return s + 's'; if (s < 3600) return Math.floor(s / 60) + 'm ' + (s % 60) + 's'; return Math.floor(s / 3600) + 'h ' + Math.floor((s % 3600) / 60) + 'm'; };
const fmtTime = (ms) => ms ? new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' }) : '—';
const pct = (j) => j.total ? Math.min(100, j.done / j.total * 100) : (j.state === 'completed' ? 100 : 0);
const KIND = { lan_send: 'Envío LAN', lan_receive: 'Recepción LAN', global_upload: 'Subida (link)', download: 'Descarga' };
const STATE = { running: 'En curso', queued: 'En cola', completed: 'Completada', failed: 'Fallida', cancelled: 'Cancelada' };
const ICON = {
  lan_send: '<svg viewBox="0 0 24 24"><path d="M12 19V7M7 12l5-5 5 5"/></svg>', lan_receive: '<svg viewBox="0 0 24 24"><path d="M12 5v12M7 12l5 5 5-5"/></svg>',
  global_upload: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M12 16V8M9 11l3-3 3 3"/></svg>', download: '<svg viewBox="0 0 24 24"><path d="M12 4v11M8 11l4 4 4-4M4 20h16"/></svg>',
};
async function api(path, opts = {}) {
  const r = await fetch(path, { headers: { 'Content-Type': 'application/json' }, ...opts, body: opts.body != null ? JSON.stringify(opts.body) : undefined });
  const text = await r.text(); let data = null; try { data = text ? JSON.parse(text) : null; } catch { data = { raw: text }; }
  if (!r.ok) throw new Error((data && data.error) || r.statusText || 'error');
  return data;
}
function toast(msg, type = 'info', ms = 4500) {
  const el = document.createElement('div'); el.className = 'toast ' + type;
  el.innerHTML = `<div style="flex:1">${h(msg)}</div><span class="x">✕</span>`; el.querySelector('.x').onclick = () => el.remove();
  $('#toasts').appendChild(el); if (ms) setTimeout(() => el.remove(), ms);
}
async function copy(text) { try { await navigator.clipboard.writeText(text); toast('Copiado al portapapeles', 'ok', 2000); } catch { toast('No se pudo copiar', 'err'); } }

const S = { data: { jobs: [], pending: [], devices: [] }, filter: localStorage.filter || 'all', q: '', sel: null, sort: JSON.parse(localStorage.sort || '{"k":"started","d":-1}'), tab: 'general', history: [], online: true,
  cols: JSON.parse(localStorage.cols || 'null') || ['name', 'kind', 'size', 'progress', 'state', 'speed', 'eta', 'peer', 'started'] };
const COLS = { name: ['Nombre', '32%'], kind: ['Tipo', '110px'], size: ['Tamaño', '95px'], progress: ['Progreso', '22%'], state: ['Estado', '105px'], speed: ['Velocidad', '105px'], eta: ['ETA', '80px'], peer: ['Origen / destino', '18%'], started: ['Inicio', '85px'] };
const FILTERS = [
  ['all', 'Todas', '<svg viewBox="0 0 24 24"><path d="M4 6h16M4 12h16M4 18h16"/></svg>', () => true],
  ['active', 'Activas', '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 3"/></svg>', j => j.state === 'running' || j.state === 'queued'],
  ['receiving', 'Recibiendo', ICON.lan_receive, j => j.kind === 'lan_receive'], ['sending', 'Enviando LAN', ICON.lan_send, j => j.kind === 'lan_send'],
  ['uploads', 'Subidas (links)', ICON.global_upload, j => j.kind === 'global_upload'], ['downloads', 'Descargas', ICON.download, j => j.kind === 'download'],
  ['completed', 'Completadas', '<svg viewBox="0 0 24 24"><path d="M5 13l4 4L19 7"/></svg>', j => j.state === 'completed'],
  ['failed', 'Fallidas', '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M15 9l-6 6M9 9l6 6"/></svg>', j => j.state === 'failed' || j.state === 'cancelled'],
];
const job = (id) => S.data.jobs.find(j => j.id === id);
function visibleJobs() {
  const f = FILTERS.find(x => x[0] === S.filter) || FILTERS[0], q = S.q.trim().toLowerCase();
  let list = S.data.jobs.filter(f[3]);
  if (q) list = list.filter(j => (j.name + ' ' + j.peer + ' ' + (j.link || '') + ' ' + KIND[j.kind]).toLowerCase().includes(q));
  const { k, d } = S.sort, val = (j) => ({ name: j.name.toLowerCase(), kind: j.kind, size: j.total, progress: pct(j), state: j.state, speed: j.speed, eta: j.eta ?? 1e15, peer: j.peer.toLowerCase(), started: j.started }[k]);
  return list.sort((a, b) => { const x = val(a), y = val(b); return (x > y ? 1 : x < y ? -1 : 0) * d; });
}
function renderSidebar() {
  $('#filters').innerHTML = FILTERS.map(f => `<div class="side-item ${S.filter === f[0] ? 'active' : ''}" data-f="${f[0]}">${f[2]}<span class="lbl">${f[1]}</span><span class="cnt">${S.data.jobs.filter(f[3]).length}</span></div>`).join('');
  $$('#filters .side-item').forEach(el => el.onclick = () => { S.filter = el.dataset.f; localStorage.filter = S.filter; render(); });
  const devs = S.data.devices || [];
  $('#devices').innerHTML = devs.length ? devs.map(d => `<div class="side-item dev" data-name="${h(d.name)}"><span class="dot ${d.requires_pin ? 'pin' : ''}"></span><span class="lbl"><span>${h(d.name)}</span><span class="sub">${h((d.addresses[0] || '?') + ':' + d.port)}${d.requires_pin ? ' · PIN' : ''}</span></span></div>`).join('')
    : `<div class="side-empty">Ningún dispositivo encontrado.<br>Abre <b>uni-share gui</b> o <b>receive</b> en el otro equipo.</div>`;
  $$('#devices .dev').forEach(el => el.onclick = () => openNew('lan', { device: el.dataset.name }));
  const d = S.data;
  $('#me').innerHTML = `<b>${h(d.device_name || '')}</b><code title="Huella TLS">${h(d.fingerprint || '')}</code><div class="row"><span>LAN</span><code>${h((d.local_ips || [])[0] || '')}:${d.lan_port || ''}</code></div><div class="row"><span>${d.pin_required ? 'PIN requerido' : 'Sin PIN'}</span><span>${d.auto_accept ? 'auto-aceptar' : 'confirmar'}</span></div>`;
}
function renderOffers() {
  const p = S.data.pending || [];
  $('#offers').innerHTML = p.map(o => `<div class="offer" data-id="${h(o.transfer_id)}"><div class="ic"><svg viewBox="0 0 24 24"><path d="M12 5v12M7 12l5 5 5-5"/></svg></div>
    <div><div class="t">${h(o.sender)} quiere enviarte <u>${h(o.name)}</u></div><div class="s">${fmtB(o.total_size)} · ${o.files.length} archivo(s)${o.compressed ? ' · comprimido' : ''} · desde <code>${h(o.peer)}</code> · huella <code>${h(o.sender_fingerprint)}</code> <a href="#" data-act="preview">ver archivos</a></div></div>
    <div class="actions"><button class="btn" data-act="reject">Rechazar</button><button class="btn" data-act="dest">Carpeta…</button><button class="btn primary" data-act="accept">Aceptar</button></div></div>`).join('');
  $$('#offers .offer').forEach(el => {
    const o = p.find(x => x.transfer_id === el.dataset.id), err = (e) => toast(e.message, 'err');
    $('[data-act=accept]', el).onclick = () => api(`/api/offers/${o.transfer_id}/accept`, { method: 'POST', body: {} }).then(() => toast(`Recibiendo ${o.name}`, 'ok')).catch(err);
    $('[data-act=reject]', el).onclick = () => api(`/api/offers/${o.transfer_id}/reject`, { method: 'POST' }).catch(err);
    $('[data-act=dest]', el).onclick = () => pickPath({ dirsOnly: true, title: 'Carpeta de destino' }).then(dir => dir && api(`/api/offers/${o.transfer_id}/accept`, { method: 'POST', body: { dest: dir } })).catch(err);
    $('[data-act=preview]', el).onclick = (ev) => { ev.preventDefault(); modal(`<div class="m-head"><h2>${h(o.name)} — ${o.files.length} archivo(s), ${fmtB(o.total_size)}</h2><button class="btn icon ghost" data-close>✕</button></div><div class="m-body"><div class="preview">${o.files.map(f => `${h(f.path)}  <span class="muted">(${fmtB(f.size)})</span>`).join('<br>')}</div></div>`); };
  });
}
function renderTable() {
  const list = visibleJobs();
  $('#thead').innerHTML = S.cols.map(c => `<th data-k="${c}" style="width:${COLS[c][1]}">${COLS[c][0]}${S.sort.k === c ? `<span class="arrow">${S.sort.d > 0 ? '▲' : '▼'}</span>` : ''}</th>`).join('');
  $$('#thead th').forEach(th => th.onclick = () => { const k = th.dataset.k; S.sort = { k, d: S.sort.k === k ? -S.sort.d : (k === 'started' ? -1 : 1) }; localStorage.sort = JSON.stringify(S.sort); render(); });
  const cell = (j, c) => ({
    name: () => `<td class="name" title="${h(j.name)}"><span class="kind ${j.kind}">${ICON[j.kind] || ''}</span>${h(j.name)}</td>`,
    kind: () => `<td class="muted">${KIND[j.kind] || j.kind}</td>`, size: () => `<td class="mono">${j.total ? fmtB(j.total) : '—'}</td>`,
    progress: () => { const p = pct(j), ind = j.state === 'running' && !j.total; return `<td><div class="prog"><div class="bar ${j.state} ${ind ? 'indet' : ''}"><i style="width:${p.toFixed(1)}%"></i></div><span class="pct">${ind ? '…' : p.toFixed(p < 10 && p > 0 ? 1 : 0) + '%'}</span></div></td>`; },
    state: () => `<td><span class="badge ${j.state}">${STATE[j.state] || j.state}</span></td>`,
    speed: () => `<td class="mono">${j.state === 'running' && j.speed ? fmtRate(j.speed) : '—'}</td>`, eta: () => `<td class="mono">${j.state === 'running' ? fmtEta(j.eta) : '—'}</td>`,
    peer: () => `<td class="muted" title="${h(j.peer)}">${h(j.peer)}</td>`, started: () => `<td class="mono muted">${fmtTime(j.started)}</td>`,
  })[c]();
  $('#rows').innerHTML = list.map(j => `<tr data-id="${j.id}" class="${S.sel === j.id ? 'sel' : ''}">${S.cols.map(c => cell(j, c)).join('')}</tr>`).join('');
  $$('#rows tr').forEach(tr => {
    tr.onclick = () => { S.sel = Number(tr.dataset.id); render(); };
    tr.ondblclick = () => { const j = job(S.sel); if (j && (j.link || j.ticket_uri)) openShare(j); else if (j && j.dest) api('/api/open', { method: 'POST', body: { path: j.dest } }).catch(() => {}); };
    tr.oncontextmenu = (e) => { e.preventDefault(); S.sel = Number(tr.dataset.id); render(); contextMenu(e.clientX, e.clientY, job(S.sel)); };
  });
  $('#empty').hidden = list.length > 0 || (S.data.pending || []).length > 0;
  const j = job(S.sel); $('#b-cancel').disabled = !(j && (j.state === 'running' || j.state === 'queued'));
}
function renderDetails() {
  const j = job(S.sel), pane = $('#pane');
  $$('#tabs .tab').forEach(t => t.classList.toggle('active', t.dataset.tab === S.tab));
  if (S.tab === 'history') return renderHistory(pane);
  if (!j) { pane.innerHTML = `<div class="muted">Selecciona una transferencia para ver sus detalles.</div>`; return; }
  if (S.tab === 'general') {
    pane.innerHTML = `<div class="two"><dl class="kv"><dt>Nombre</dt><dd>${h(j.name)}</dd><dt>Tipo</dt><dd>${KIND[j.kind] || j.kind}</dd>
      <dt>Estado</dt><dd><span class="badge ${j.state}">${STATE[j.state] || j.state}</span> <span class="muted">${h(j.message)}</span></dd>
      <dt>Progreso</dt><dd class="mono">${fmtB(j.done)} / ${j.total ? fmtB(j.total) : '?'} (${pct(j).toFixed(1)}%)</dd>
      <dt>Velocidad</dt><dd class="mono">${j.state === 'running' ? fmtRate(j.speed) + ' · ETA ' + fmtEta(j.eta) : '—'}</dd><dt>Archivo actual</dt><dd class="mono">${h(j.current_file) || '—'}</dd></dl>
      <dl class="kv"><dt>${j.kind === 'lan_send' || j.kind === 'global_upload' ? 'Destino' : 'Origen'}</dt><dd>${h(j.peer)}</dd><dt>Carpeta</dt><dd>${j.dest ? `<a href="#" id="open-dest">${h(j.dest)}</a>` : '—'}</dd>
      <dt>Archivos</dt><dd>${j.files.length || '—'}</dd><dt>Inicio</dt><dd class="mono">${fmtTime(j.started)}</dd><dt>Fin</dt><dd class="mono">${fmtTime(j.finished)}</dd>
      <dt>Link</dt><dd>${j.link ? `<a href="${h(j.link)}" target="_blank" rel="noopener">${h(j.link)}</a>` : '—'}</dd></dl></div>`;
    const od = $('#open-dest'); if (od) od.onclick = (e) => { e.preventDefault(); api('/api/open', { method: 'POST', body: { path: j.dest } }).catch(er => toast(er.message, 'err')); };
  } else if (S.tab === 'files') {
    pane.innerHTML = j.files.length ? `<table class="filelist"><thead><tr><th>Ruta</th><th style="text-align:right">Tamaño</th></tr></thead><tbody>${j.files.slice(0, 3000).map(f => `<tr><td title="${h(f.path)}">${h(f.path)}</td><td class="sz">${fmtB(f.size)}</td></tr>`).join('')}</tbody></table>` : `<div class="muted">Sin lista de archivos.</div>`;
  } else if (S.tab === 'share') {
    if (!j.link && !j.ticket_uri) { pane.innerHTML = `<div class="muted">Esta transferencia no tiene link. Usa <b>Compartir link</b> o <b>Ticket</b>.</div>`; return; }
    pane.innerHTML = `<div class="two"><div>${j.link ? `<h3 class="hint">Link público</h3><div class="linkbox"><input readonly value="${h(j.link)}"><button class="btn" id="cp-link">Copiar</button><button class="btn" id="qr-link">QR</button></div>` : ''}
      ${j.ticket_uri ? `<h3 class="hint" style="margin-top:14px">Ticket .unishare ${j.ticket_path ? `<span class="muted">(${h(j.ticket_path)})</span>` : ''}</h3><div class="linkbox"><input readonly value="${h(j.ticket_uri)}"><button class="btn" id="cp-tk">Copiar</button><button class="btn" id="qr-tk">QR</button><a class="btn" href="/api/ticket/file?uri=${encodeURIComponent(j.ticket_uri)}" download>Guardar…</a></div>` : ''}</div>
      <div style="display:flex;gap:16px">${j.link ? `<img class="qr-inline" src="/api/qr?data=${encodeURIComponent(j.link)}" title="QR del link">` : ''}${j.ticket_uri ? `<img class="qr-inline" src="/api/qr?data=${encodeURIComponent(j.ticket_uri)}" title="QR del ticket">` : ''}</div></div>`;
    const b = (id, fn) => { const e = $(id); if (e) e.onclick = fn; };
    b('#cp-link', () => copy(j.link)); b('#qr-link', () => openShare(j, 'link')); b('#cp-tk', () => copy(j.ticket_uri)); b('#qr-tk', () => openShare(j, 'ticket'));
  } else if (S.tab === 'log') { pane.innerHTML = `<pre class="log">${h(j.log.join('\n')) || '—'}</pre>`; pane.scrollTop = pane.scrollHeight; }
}
function renderHistory(pane) {
  pane.innerHTML = `<div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px"><span class="muted">${S.history.length} registro(s)</span><div><button class="btn" id="h-refresh">Actualizar</button> <button class="btn danger" id="h-clear">Borrar historial</button></div></div>
    <table class="filelist"><thead><tr><th>Fecha</th><th>Tipo</th><th>Nombre</th><th style="text-align:right">Tamaño</th><th>Estado</th><th>Origen / link</th></tr></thead><tbody>
    ${S.history.map(r => `<tr><td class="mono muted">${h(new Date(r.timestamp).toLocaleString())}</td><td>${h(r.kind)}</td><td title="${h(r.name)}">${h(r.name)}</td><td class="sz">${fmtB(r.size)}</td><td><span class="badge ${({ completed: 'completed', failed: 'failed', rejected: 'cancelled', in_progress: 'running' })[r.status] || ''}">${h(r.status)}</span></td><td title="${h(r.peer_or_link)}">${/^https?:/.test(r.peer_or_link) ? `<a href="${h(r.peer_or_link)}" target="_blank" rel="noopener">${h(r.peer_or_link)}</a>` : h(r.peer_or_link)}</td></tr>`).join('')}</tbody></table>`;
  $('#h-refresh').onclick = loadHistory;
  $('#h-clear').onclick = () => confirmDlg('¿Borrar todo el historial?').then(ok => ok && api('/api/history', { method: 'DELETE' }).then(loadHistory));
}
async function loadHistory() { try { S.history = await api('/api/history'); } catch { S.history = []; } if (S.tab === 'history') renderDetails(); }
function renderStatus() {
  const d = S.data; $('#sp-down').textContent = fmtRate(d.speed_down || 0); $('#sp-up').textContent = fmtRate(d.speed_up || 0);
  const act = (d.jobs || []).filter(j => j.state === 'running').length; $('#st-active').textContent = act ? `${act} activa(s)` : '';
  $('#st-lan').textContent = d.lan_addr ? `LAN ${d.lan_addr}` : ''; $('#st-dir').textContent = d.download_dir ? `↓ ${d.download_dir}` : ''; $('#ver').textContent = d.version ? 'v' + d.version : '';
  $('#conn').className = 'conn' + (S.online ? '' : ' off'); $('#conn span').textContent = S.online ? 'conectado' : 'sin conexión con el proceso';
}
function render() { renderSidebar(); renderOffers(); renderTable(); renderDetails(); renderStatus(); }

let lastPending = 0, lastStates = new Map();
async function poll() {
  try {
    const d = await api('/api/state'); S.online = true;
    for (const j of d.jobs) {
      const prev = lastStates.get(j.id);
      if (prev && prev !== j.state && (j.state === 'completed' || j.state === 'failed')) { toast(`${KIND[j.kind]} «${j.name}»: ${STATE[j.state]}${j.state === 'failed' ? ' — ' + j.message : ''}`, j.state === 'completed' ? 'ok' : 'err', 6000); if (j.state === 'completed' && j.link) { S.sel = j.id; S.tab = 'share'; } }
      lastStates.set(j.id, j.state);
    }
    if (d.pending.length > lastPending) toast('Solicitud de transferencia entrante', 'info');
    lastPending = d.pending.length; S.data = d;
    if (S.sel && !d.jobs.some(j => j.id === S.sel)) S.sel = null;
  } catch { S.online = false; }
  render();
  const fast = S.data.jobs?.some(j => j.state === 'running') || S.data.pending?.length;
  setTimeout(poll, document.hidden ? 4000 : fast ? 700 : 1800);
}

function modal(html, cls = '') {
  const ov = document.createElement('div'); ov.className = 'overlay'; ov.innerHTML = `<div class="modal ${cls}" role="dialog">${html}</div>`;
  const close = () => { ov.remove(); document.removeEventListener('keydown', esc); ov.dispatchEvent(new Event('closed')); }, esc = (e) => { if (e.key === 'Escape') close(); };
  document.addEventListener('keydown', esc); ov.addEventListener('mousedown', (e) => { if (e.target === ov) close(); });
  $$('[data-close]', ov).forEach(b => b.onclick = close); $('#modals').appendChild(ov);
  const first = ov.querySelector('input:not([readonly]),textarea,select,button.primary'); if (first) setTimeout(() => first.focus(), 30);
  ov.close = close; return ov;
}
function confirmDlg(msg) { return new Promise(res => { const m = modal(`<div class="m-head"><h2>Confirmar</h2></div><div class="m-body">${h(msg)}</div><div class="m-foot"><button class="btn" id="c-no">Cancelar</button><button class="btn primary" id="c-yes">Aceptar</button></div>`, 'narrow'); $('#c-no', m).onclick = () => { m.close(); res(false); }; $('#c-yes', m).onclick = () => { m.close(); res(true); }; }); }

function pickPath({ dirsOnly = false, title = 'Seleccionar', start = '' } = {}) {
  return new Promise((resolve) => {
    const m = modal(`<div class="m-head"><h2>${h(title)}</h2><button class="btn icon ghost" data-close>✕</button></div><div class="m-body"><div class="fb"><div class="roots" id="fb-roots"></div><div class="list">
      <div class="path"><input class="input" id="fb-path" placeholder="Ruta…"><button class="btn" id="fb-up" title="Subir">↑</button></div><div class="entries" id="fb-entries"></div></div></div><div class="hint" id="fb-sel"></div></div>
      <div class="m-foot"><button class="btn" data-close>Cancelar</button><button class="btn primary" id="fb-ok">Seleccionar</button></div>`, 'wide');
    let cur = start, sel = null, parent = null, done = false; const fin = (v) => { if (!done) { done = true; resolve(v); } };
    m.addEventListener('closed', () => fin(null));
    const load = async (p) => { try {
      const d = await api('/api/fs?dirs_only=' + dirsOnly + '&path=' + encodeURIComponent(p || '')); cur = d.path; parent = d.parent; sel = dirsOnly ? cur : null; $('#fb-path', m).value = cur;
      $('#fb-roots', m).innerHTML = d.roots.map(r => `<div title="${h(r)}">${h(String(r).replace(/[\\/]$/, '').split(/[\\/]/).pop() || r)}</div>`).join('');
      $$('#fb-roots div', m).forEach((el, i) => el.onclick = () => load(d.roots[i]));
      $('#fb-entries', m).innerHTML = d.entries.map((e, i) => `<div class="e ${e.dir ? 'dir' : ''}" data-i="${i}">${e.dir ? '<svg viewBox="0 0 24 24"><path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z"/></svg>' : '<svg viewBox="0 0 24 24"><path d="M6 3h8l4 4v14H6z M14 3v4h4"/></svg>'}<span class="n">${h(e.name)}</span><span class="sz">${e.dir ? '' : fmtB(e.size)}</span></div>`).join('') || '<div class="side-empty">Carpeta vacía</div>';
      $$('#fb-entries .e', m).forEach(el => { const e = d.entries[Number(el.dataset.i)]; el.onclick = () => { $$('#fb-entries .e', m).forEach(x => x.classList.remove('sel')); el.classList.add('sel'); sel = e.path; $('#fb-sel', m).textContent = sel; }; el.ondblclick = () => { if (e.dir) load(e.path); else { fin(e.path); m.close(); } }; });
      $('#fb-sel', m).textContent = sel || '';
    } catch (e) { toast(e.message, 'err'); } };
    $('#fb-up', m).onclick = () => parent && load(parent); $('#fb-path', m).onkeydown = (e) => { if (e.key === 'Enter') load(e.target.value); };
    $('#fb-ok', m).onclick = () => { const v = sel || $('#fb-path', m).value; if (v) { fin(v); m.close(); } };
    load(start);
  });
}

function openNew(mode = 'lan', preset = {}) {
  const m = modal(`<div class="m-head"><h2>Nueva transferencia</h2><div class="seg" id="nt-seg"><button data-m="lan">Enviar por LAN</button><button data-m="global">Compartir link</button><button data-m="download">Descargar</button><button data-m="ticket">Crear ticket</button></div><button class="btn icon ghost" data-close>✕</button></div>
    <div class="m-body" id="nt-body"></div><div class="m-foot"><span id="nt-err" style="flex:1;color:var(--err)"></span><button class="btn" data-close>Cancelar</button><button class="btn primary" id="nt-go">Iniciar</button></div>`);
  const body = $('#nt-body', m); let cur = mode, devSel = preset.device || null;
  const srcField = () => `<div class="field"><label>Archivo o carpeta</label><div class="with-btn"><input type="text" id="f-path" placeholder="/ruta/al/archivo-o-carpeta" value="${h(preset.path || '')}"><button class="btn" id="f-browse">Explorar…</button></div><div class="preview" id="f-prev" hidden></div></div>`;
  const draw = () => {
    $$('#nt-seg button', m).forEach(b => b.classList.toggle('active', b.dataset.m === cur));
    if (cur === 'lan') body.innerHTML = srcField() + `<div class="field"><label>Dispositivo destino</label><div class="devpick" id="f-devs"></div></div><div class="grid2"><div class="field"><label>o dirección manual <span class="hint">ip[:puerto]</span></label><input type="text" id="f-target" placeholder="192.168.1.20:47820"></div><div class="field"><label>PIN (si lo exige el receptor)</label><input type="text" id="f-pin" inputmode="numeric"></div></div><label class="check"><input type="checkbox" id="f-compress"> Comprimir carpeta en .tar.zst antes de enviar</label>`;
    else if (cur === 'global') body.innerHTML = srcField() + `<div class="grid2"><div class="field"><label>Contraseña (opcional, 4-100)</label><input type="password" id="f-pw" autocomplete="new-password"></div><div class="field"><label>Caducidad (días, 1-7)</label><input type="number" id="f-exp" min="1" max="7" value="${S.cfg?.global?.expiry_days || 7}"></div><div class="field"><label>Máx. descargas (opcional)</label><input type="number" id="f-max" min="1" max="1000" placeholder="∞"></div><div class="field"><label>Mensaje para el ticket</label><input type="text" id="f-msg"></div></div>
      <label class="check"><input type="checkbox" id="f-ticket" checked> Generar ticket <b>.unishare</b> (link + contraseña + digests BLAKE3, compartible por QR)</label><label class="check"><input type="checkbox" id="f-compress"> Comprimir carpeta en un único .tar.zst (en vez de colección)</label>
      <div class="alert">Se sube a <b>storage.to</b> (≤ 25 GB, anónimo). El link y el QR aparecerán en <b>Compartir</b> al terminar.</div>`;
    else if (cur === 'download') body.innerHTML = `<div class="field"><label>Link, URI <code>unishare:</code> o ruta a un fichero .unishare</label><textarea id="f-url" placeholder="https://storage.to/…   ·   https://www.swisstransfer.com/d/…   ·   unishare:…">${h(preset.url || '')}</textarea><div class="preview" id="f-prev" hidden></div></div>
      <div class="grid2"><div class="field"><label>Contraseña (si la pide)</label><input type="password" id="f-pw"></div><div class="field"><label>Carpeta destino</label><div class="with-btn"><input type="text" id="f-dest" value="${h(S.data.download_dir || '')}"><button class="btn" id="f-browse-d">…</button></div></div></div><label class="check"><input type="checkbox" id="f-force"> Sobrescribir si existe (por defecto se renombra <i>archivo (1).ext</i>)</label>`;
    else body.innerHTML = `<div class="field"><label>Links (uno por línea; se prueban en orden)</label><textarea id="f-links" placeholder="https://storage.to/abc123&#10;https://www.swisstransfer.com/d/…"></textarea></div><div class="grid2"><div class="field"><label>Título</label><input type="text" id="f-name"></div><div class="field"><label>Contraseña embebida (opcional)</label><input type="password" id="f-pw"></div></div><div class="field"><label>Mensaje</label><input type="text" id="f-msg"></div>
      <div class="field"><label>Copia local para añadir digests BLAKE3 (opcional)</label><div class="with-btn"><input type="text" id="f-path" placeholder="/ruta/original"><button class="btn" id="f-browse">Explorar…</button></div></div><div class="alert">Un <b>ticket .unishare</b> agrupa links alternativos, contraseña y hashes en un solo fichero/QR.</div>`;
    const br = $('#f-browse', m); if (br) br.onclick = () => pickPath({ title: 'Archivo o carpeta a compartir' }).then(p => { if (p) { $('#f-path', m).value = p; if (cur !== 'ticket') preview(p); } });
    const brd = $('#f-browse-d', m); if (brd) brd.onclick = () => pickPath({ dirsOnly: true, title: 'Carpeta destino' }).then(p => p && ($('#f-dest', m).value = p));
    const fp = $('#f-path', m); if (fp && cur !== 'ticket') { fp.onchange = () => preview(fp.value); if (fp.value) preview(fp.value); }
    const fu = $('#f-url', m); if (fu) { fu.oninput = () => previewTicket(fu.value); if (fu.value) previewTicket(fu.value); }
    if (cur === 'lan') drawDevs();
  };
  const preview = async (p) => { const pv = $('#f-prev', m); if (!pv || !p) return; pv.hidden = false; pv.innerHTML = '<span class="muted">Analizando…</span>';
    try { const d = await api('/api/preview?path=' + encodeURIComponent(p)); pv.innerHTML = `<div class="sum">${d.count} archivo(s) · ${fmtB(d.total)}</div>` + d.files.slice(0, 200).map(f => `${h(f.path)} <span class="muted">(${fmtB(f.size)})</span>`).join('<br>') + (d.count > 200 ? `<br>… y ${d.count - 200} más` : ''); } catch (e) { pv.innerHTML = `<span style="color:var(--err)">${h(e.message)}</span>`; } };
  const previewTicket = async (v) => { const pv = $('#f-prev', m); v = v.trim(); if (!pv) return; if (!/^unishare:|\.unishare$|^\{/.test(v)) { pv.hidden = true; return; }
    try { const d = await api('/api/ticket/parse', { method: 'POST', body: { data: v } }); pv.hidden = false; pv.innerHTML = `<div class="sum">🎫 ${h(d.summary)}${d.expired ? ' <span style="color:var(--err)">(expirado)</span>' : ''}</div>` + d.ticket.sources.map(s => `[${s.kind}] ${h(s.url)}${s.password ? ' 🔒' : ''}`).join('<br>') + (d.ticket.files.length ? '<br>' + d.ticket.files.slice(0, 100).map(f => `${h(f.path)} <span class="muted">(${fmtB(f.size)})${f.blake3 ? ' ✓' : ''}</span>`).join('<br>') : ''); } catch (e) { pv.hidden = false; pv.innerHTML = `<span style="color:var(--err)">${h(e.message)}</span>`; } };
  const drawDevs = () => { const devs = S.data.devices || []; $('#f-devs', m).innerHTML = devs.length ? devs.map(d => `<div class="devcard ${devSel === d.name ? 'active' : ''}" data-n="${h(d.name)}"><b>${h(d.name)}</b><small>${h((d.addresses[0] || '?') + ':' + d.port)}${d.requires_pin ? ' · PIN' : ''}</small></div>`).join('') : '<div class="side-empty">No se han encontrado dispositivos. Usa la dirección manual.</div>'; $$('#f-devs .devcard', m).forEach(el => el.onclick = () => { devSel = el.dataset.n; drawDevs(); }); };
  $$('#nt-seg button', m).forEach(b => b.onclick = () => { cur = b.dataset.m; draw(); });
  $('#nt-go', m).onclick = async () => {
    const err = $('#nt-err', m); err.textContent = '';
    try {
      const v = (id) => ($(id, m) || {}).value?.trim(), c = (id) => !!($(id, m) || {}).checked;
      if (cur === 'lan') { const dev = (S.data.devices || []).find(d => d.name === devSel); const target = v('#f-target') || (dev ? `${dev.addresses[0]}:${dev.port}` : ''); if (!v('#f-path')) throw new Error('Indica el archivo o carpeta'); if (!target) throw new Error('Elige un dispositivo o escribe una dirección');
        await api('/api/send-lan', { method: 'POST', body: { path: v('#f-path'), target, fingerprint: dev?.fingerprint || null, pin: v('#f-pin') || null, compress: c('#f-compress') } }); toast('Envío LAN iniciado', 'ok'); }
      else if (cur === 'global') { if (!v('#f-path')) throw new Error('Indica el archivo o carpeta'); await api('/api/send-global', { method: 'POST', body: { path: v('#f-path'), password: v('#f-pw') || null, expiry_days: Number(v('#f-exp')) || null, max_downloads: Number(v('#f-max')) || null, compress: c('#f-compress'), ticket: c('#f-ticket'), message: v('#f-msg') || null } }); toast('Subida iniciada', 'ok'); }
      else if (cur === 'download') { if (!v('#f-url')) throw new Error('Pega un link o ticket'); await api('/api/download', { method: 'POST', body: { url: v('#f-url'), password: v('#f-pw') || null, dest: v('#f-dest') || null, force: c('#f-force') } }); toast('Descarga iniciada', 'ok'); }
      else { const links = v('#f-links').split(/\s+/).filter(Boolean); if (!links.length) throw new Error('Añade al menos un link'); const r = await api('/api/ticket/create', { method: 'POST', body: { links, name: v('#f-name') || null, password: v('#f-pw') || null, message: v('#f-msg') || null, verify_from: v('#f-path') || null } }); m.close(); openShare({ name: r.ticket.name, ticket_uri: r.uri, ticket_path: r.path, link: r.ticket.sources[0].url }, 'ticket'); return; }
      S.filter = 'active'; localStorage.filter = 'active'; m.close();
    } catch (e) { err.textContent = e.message; }
  };
  draw();
}

function openShare(j, what = j.ticket_uri ? 'ticket' : 'link') {
  const items = []; if (j.link) items.push(['link', 'Link público', j.link]); if (j.ticket_uri) items.push(['ticket', 'Ticket .unishare', j.ticket_uri]);
  if (!items.length) return toast('Nada que compartir', 'info');
  let cur = items.find(i => i[0] === what) ? what : items[0][0];
  const m = modal(`<div class="m-head"><h2>Compartir «${h(j.name)}»</h2>${items.length > 1 ? `<div class="seg" id="sh-seg">${items.map(i => `<button data-w="${i[0]}">${i[1]}</button>`).join('')}</div>` : ''}<button class="btn icon ghost" data-close>✕</button></div><div class="m-body" id="sh-body"></div><div class="m-foot"><button class="btn" data-close>Cerrar</button></div>`, 'wide');
  const draw = () => {
    const it = items.find(i => i[0] === cur); $$('#sh-seg button', m).forEach(b => b.classList.toggle('active', b.dataset.w === cur));
    $('#sh-body', m).innerHTML = `<div class="share"><div class="qr"><img src="/api/qr?data=${encodeURIComponent(it[2])}" alt="QR"></div><div><h3>${it[1]}</h3><div class="linkbox"><input readonly value="${h(it[2])}"><button class="btn" id="sh-cp">Copiar</button></div>
      ${cur === 'ticket' ? `<p class="hint">El QR/URI contiene los links alternativos${it[2].length > 900 ? ' (lista de archivos resumida para caber en el QR)' : ', la contraseña y los digests BLAKE3'}. El destinatario lo escanea o abre el fichero .unishare.</p><div style="display:flex;gap:8px;flex-wrap:wrap"><a class="btn" href="/api/ticket/file?uri=${encodeURIComponent(it[2])}" download="${h(j.name)}.unishare">⬇ Guardar .unishare</a>${j.ticket_path ? `<button class="btn" id="sh-open">Abrir carpeta del ticket</button>` : ''}<a class="btn" href="/api/qr?data=${encodeURIComponent(it[2])}" download="qr-${h(j.name)}.svg">Guardar QR (SVG)</a></div>`
        : `<p class="hint">Cualquiera con el link puede descargar (si tiene contraseña, la pedirá). Escanea el QR con el móvil.</p><div style="display:flex;gap:8px;flex-wrap:wrap"><a class="btn" href="${h(it[2])}" target="_blank" rel="noopener">Abrir en el navegador</a><a class="btn" href="/api/qr?data=${encodeURIComponent(it[2])}" download="qr.svg">Guardar QR (SVG)</a></div>`}</div></div>`;
    $('#sh-cp', m).onclick = () => copy(it[2]); const so = $('#sh-open', m); if (so) so.onclick = () => api('/api/open', { method: 'POST', body: { path: j.ticket_path } }).catch(e => toast(e.message, 'err'));
  };
  $$('#sh-seg button', m).forEach(b => b.onclick = () => { cur = b.dataset.w; draw(); }); draw();
}

async function openSettings() {
  let d; try { d = await api('/api/config'); } catch (e) { return toast(e.message, 'err'); }
  const c = d.config; S.cfg = c;
  const m = modal(`<div class="m-head"><h2>Ajustes</h2><span class="hint mono">${h(d.path)}</span><button class="btn icon ghost" data-close>✕</button></div><div class="m-body settings"><h3>Dispositivo</h3>
    <div class="grid2"><div class="field"><label>Nombre visible en la LAN</label><input type="text" id="s-name" value="${h(c.device_name)}"></div><div class="field"><label>PIN de emparejamiento (vacío = sin PIN)</label><input type="text" id="s-pin" value="${h(c.pin || '')}" placeholder="4-6 dígitos"></div></div>
    <div class="field"><label>Carpeta de descargas</label><div class="with-btn"><input type="text" id="s-dir" value="${h(c.download_dir)}"><button class="btn" id="s-browse">…</button></div></div>
    <div class="grid2"><div class="field"><label>Límite de velocidad (Mbit/s, 0 = sin límite)</label><input type="number" id="s-rate" min="0" value="${c.rate_limit_mbps}"></div><div class="field"><label>Partes paralelas (multipart)</label><input type="number" id="s-par" min="1" max="16" value="${c.global.parallel_parts}"></div></div>
    <label class="check"><input type="checkbox" id="s-auto" ${c.auto_accept ? 'checked' : ''}> Aceptar automáticamente las transferencias LAN entrantes</label><label class="check"><input type="checkbox" id="s-notif" ${c.notifications ? 'checked' : ''}> Notificaciones de escritorio</label><label class="check"><input type="checkbox" id="s-comp" ${c.compress_folders ? 'checked' : ''}> Comprimir carpetas (.tar.zst) por defecto</label>
    <h3>Links (storage.to)</h3><div class="grid2"><div class="field"><label>Caducidad por defecto (días, 1-7)</label><input type="number" id="s-exp" min="1" max="7" value="${c.global.expiry_days}"></div><div class="field"><label>Puerto LAN (requiere reiniciar)</label><input type="text" readonly value="${c.lan_port}"></div></div>
    <h3>Interfaz</h3><div class="field"><label>Columnas visibles</label><div style="display:flex;flex-wrap:wrap;gap:10px">${Object.keys(COLS).map(k => `<label class="check"><input type="checkbox" data-col="${k}" ${S.cols.includes(k) ? 'checked' : ''}> ${COLS[k][0]}</label>`).join('')}</div></div>
    <div class="alert">Huella TLS de este equipo: <code class="mono">${h(S.data.fingerprint_full || '')}</code></div></div>
    <div class="m-foot"><span id="s-err" style="flex:1;color:var(--err)"></span><button class="btn" data-close>Cancelar</button><button class="btn primary" id="s-save">Guardar</button></div>`);
  $('#s-browse', m).onclick = () => pickPath({ dirsOnly: true, title: 'Carpeta de descargas' }).then(p => p && ($('#s-dir', m).value = p));
  $('#s-save', m).onclick = async () => { const v = (id) => $(id, m).value.trim(), c2 = (id) => $(id, m).checked;
    try { const r = await api('/api/config', { method: 'PUT', body: { device_name: v('#s-name'), pin: v('#s-pin'), download_dir: v('#s-dir'), rate_limit_mbps: Number(v('#s-rate')) || 0, parallel_parts: Number(v('#s-par')) || 4, auto_accept: c2('#s-auto'), notifications: c2('#s-notif'), compress_folders: c2('#s-comp'), expiry_days: Number(v('#s-exp')) || 7 } });
      S.cols = Object.keys(COLS).filter(k => $(`[data-col=${k}]`, m).checked); if (!S.cols.length) S.cols = ['name', 'progress', 'state']; localStorage.cols = JSON.stringify(S.cols);
      toast(r.restart_needed ? 'Guardado. Nombre/PIN/puerto se aplican al reiniciar la GUI.' : 'Ajustes guardados', 'ok', 5000); m.close(); render(); } catch (e) { $('#s-err', m).textContent = e.message; } };
}

function contextMenu(x, y, j) {
  $$('.ctx').forEach(e => e.remove()); if (!j) return;
  const el = document.createElement('div'); el.className = 'ctx'; const act = j.state === 'running' || j.state === 'queued';
  el.innerHTML = `<div data-a="details">Detalles <kbd class="k">Enter</kbd></div><div data-a="share" class="${j.link || j.ticket_uri ? '' : 'dis'}">Compartir / QR…</div><div data-a="copy" class="${j.link ? '' : 'dis'}">Copiar link</div><div data-a="open" class="${j.dest ? '' : 'dis'}">Abrir carpeta</div><hr><div data-a="cancel" class="${act ? '' : 'dis'}">Cancelar <kbd class="k">Supr</kbd></div><div data-a="remove" class="${act ? 'dis' : ''}">Quitar de la lista</div>`;
  el.style.left = Math.min(x, innerWidth - 210) + 'px'; el.style.top = Math.min(y, innerHeight - 220) + 'px'; document.body.appendChild(el);
  const off = () => { el.remove(); document.removeEventListener('mousedown', outside); }, outside = (e) => { if (!el.contains(e.target)) off(); }; setTimeout(() => document.addEventListener('mousedown', outside), 0);
  el.onclick = (e) => { const a = e.target.closest('[data-a]')?.dataset.a; if (!a) return; off();
    if (a === 'details') { S.tab = 'general'; $('#main').classList.remove('details-collapsed'); render(); } if (a === 'share') openShare(j); if (a === 'copy') copy(j.link);
    if (a === 'open') api('/api/open', { method: 'POST', body: { path: j.dest } }).catch(er => toast(er.message, 'err')); if (a === 'cancel') cancelSel();
    if (a === 'remove') api(`/api/jobs/${j.id}`, { method: 'DELETE' }).then(() => { S.sel = null; }).catch(er => toast(er.message, 'err')); };
}
function cancelSel() { const j = job(S.sel); if (!j) return; confirmDlg(`¿Cancelar «${j.name}»?`).then(ok => ok && api(`/api/jobs/${j.id}/cancel`, { method: 'POST' }).then(() => toast('Cancelada', 'info')).catch(e => toast(e.message, 'err'))); }

async function handleDrop(e) {
  const files = [...(e.dataTransfer?.files || [])], text = (e.dataTransfer?.getData('text') || '').trim(), tk = files.find(f => /\.unishare$/i.test(f.name));
  if (tk) { const buf = new Uint8Array(await tk.arrayBuffer()); let b64 = ''; for (let i = 0; i < buf.length; i += 0x8000) b64 += String.fromCharCode.apply(null, buf.subarray(i, i + 0x8000));
    return openNew('download', { url: 'unishare:' + btoa(b64).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '') }); }
  if (text) return /^https?:|^unishare:/.test(text) ? openNew('download', { url: text }) : openNew('lan', { path: text });
  if (files.length) toast('El navegador no expone la ruta real del archivo: usa «Explorar…» o pega la ruta.', 'info', 7000);
}

function init() {
  document.documentElement.dataset.theme = localStorage.theme || 'dark';
  $('#b-theme').onclick = () => { const t = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark'; document.documentElement.dataset.theme = t; localStorage.theme = t; };
  $('#b-new').onclick = () => openNew('lan'); $('#b-sendlan').onclick = () => openNew('lan'); $('#b-upload').onclick = () => openNew('global'); $('#b-download').onclick = () => openNew('download'); $('#b-ticket').onclick = () => openNew('ticket');
  $('#b-settings').onclick = openSettings; $('#b-cancel').onclick = cancelSel;
  $('#b-clear').onclick = () => api('/api/jobs/clear-finished', { method: 'POST' }).then(r => toast(`${r.removed} eliminada(s)`, 'info', 2000));
  $('#b-scan').onclick = async () => { $('#b-scan').disabled = true; try { S.data.devices = await api('/api/devices'); render(); } finally { $('#b-scan').disabled = false; } };
  $('#q').oninput = (e) => { S.q = e.target.value; renderTable(); };
  $$('#tabs .tab').forEach(t => t.onclick = () => { S.tab = t.dataset.tab; if (S.tab === 'history') loadHistory(); renderDetails(); });
  const sp = $('#splitter'), main = $('#main'); main.style.setProperty('--details-h', (localStorage.detailsH || 260) + 'px'); if (localStorage.detailsCollapsed === '1') main.classList.add('details-collapsed');
  sp.onmousedown = (e) => { e.preventDefault(); const start = e.clientY, h0 = parseInt(getComputedStyle(main).getPropertyValue('--details-h')); const mv = (ev) => { const nh = Math.max(120, Math.min(innerHeight - 260, h0 + (start - ev.clientY))); main.style.setProperty('--details-h', nh + 'px'); localStorage.detailsH = nh; }; const up = () => { removeEventListener('mousemove', mv); removeEventListener('mouseup', up); }; addEventListener('mousemove', mv); addEventListener('mouseup', up); };
  sp.ondblclick = () => { main.classList.toggle('details-collapsed'); localStorage.detailsCollapsed = main.classList.contains('details-collapsed') ? '1' : '0'; };
  const tw = $('#tablewrap'); addEventListener('dragover', (e) => { e.preventDefault(); tw.style.outline = '2px dashed var(--accent)'; tw.style.outlineOffset = '-6px'; }); addEventListener('dragleave', () => { tw.style.outline = ''; });
  addEventListener('drop', (e) => { e.preventDefault(); tw.style.outline = ''; if (!$('.overlay')) handleDrop(e); });
  addEventListener('keydown', (e) => {
    if ($('.overlay') || e.target.matches('input,textarea,select')) { if (e.key === 'Escape' && e.target.id === 'q') { e.target.value = ''; S.q = ''; renderTable(); e.target.blur(); } return; }
    const k = e.key.toLowerCase();
    if (k === 'n' || k === 'l') openNew('lan'); else if (k === 'u') openNew('global'); else if (k === 'd') openNew('download'); else if (k === 't') openNew('ticket'); else if (k === ',') openSettings();
    else if (k === '/') { e.preventDefault(); $('#q').focus(); } else if (k === 'delete') cancelSel();
    else if (k === 'enter') { if (job(S.sel)) { main.classList.remove('details-collapsed'); S.tab = 'general'; renderDetails(); } }
    else if (k === 'arrowdown' || k === 'arrowup') { const l = visibleJobs(); if (!l.length) return; const i = l.findIndex(j => j.id === S.sel); S.sel = l[k === 'arrowdown' ? Math.min(l.length - 1, i + 1) : Math.max(0, i - 1)].id; render(); e.preventDefault(); }
    else if (k === 'escape') { S.sel = null; render(); }
  });
  poll(); loadHistory(); api('/api/config').then(d => { S.cfg = d.config; }).catch(() => {});
}
document.addEventListener('DOMContentLoaded', init);
