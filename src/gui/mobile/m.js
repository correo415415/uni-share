/* uni-share · mobile GUI (/m) — thumb-first phone layer over the same engine API as the desktop web UI.
   Sections: Inicio · Transferencias · Dispositivos · Compartir · Ajustes (+ detail views pushed on a stack).
   Bottom sheets for every flow; Android back button = pop. Works in any phone browser; window.Android adds
   camera QR, SAF folder export and system share when running inside the Kotlin shell. */
'use strict';

// ───────────────────────── helpers ─────────────────────────
const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const h = (tag, attrs = {}, ...kids) => {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v; else if (k === 'html') el.innerHTML = v; else if (k.startsWith('on')) el[k] = v; else if (v === true) el.setAttribute(k, ''); else el.setAttribute(k, v);
  }
  for (const k of kids.flat()) if (k != null) el.append(k.nodeType ? k : document.createTextNode(k));
  return el;
};
const ico = (n, cls = '') => `<svg class="${cls}" aria-hidden="true"><use href="#i-${n}"/></svg>`;
function fmtB(n) { n = Number(n) || 0; if (n < 1024) return n + ' B'; const u = ['KB', 'MB', 'GB', 'TB']; let i = -1; do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1); return (n < 10 ? n.toFixed(2) : n < 100 ? n.toFixed(1) : Math.round(n)) + ' ' + u[i]; }
function fmtS(n) { return n ? fmtB(n) + '/s' : '—'; }
function fmtT(s) { if (s == null) return '—'; s = Math.round(s); if (s < 60) return s + ' s'; if (s < 3600) return Math.floor(s / 60) + ' min ' + (s % 60) + ' s'; return Math.floor(s / 3600) + ' h ' + Math.floor((s % 3600) / 60) + ' min'; }
function fmtAgo(ms) { if (!ms) return ''; const d = Math.max(0, Date.now() - ms) / 1000; if (d < 45) return 'ahora'; if (d < 3600) return Math.round(d / 60) + ' min'; if (d < 86400) return Math.round(d / 3600) + ' h'; return Math.round(d / 86400) + ' d'; }
function fmtDate(ms) { if (!ms) return '—'; const d = new Date(ms); return d.toLocaleDateString('es', { day: '2-digit', month: 'short' }) + ' ' + d.toLocaleTimeString('es', { hour: '2-digit', minute: '2-digit' }); }
const KIND = { lan_send: 'Envío LAN', lan_receive: 'Recepción LAN', global_upload: 'Subida (link)', download: 'Descarga' };
const KICON = { lan_send: ['send', 'up'], lan_receive: ['inbox', 'down'], global_upload: ['ul', 'up'], download: ['dl', 'down'] };
const STATE = { queued: 'En cola', running: 'En curso', completed: 'Completada', failed: 'Fallida', cancelled: 'Cancelada' };
const SCAN = { info: ['ok', 'Limpio'], warning: ['warn', 'Aviso'], danger: ['danger', 'PELIGRO'] };
const isActive = j => j.state === 'queued' || j.state === 'running';
const pct = j => j.total ? Math.min(100, Math.round(j.done * 100 / j.total)) : (j.state === 'completed' ? 100 : 0);

// ───────────────────────── Android bridge ─────────────────────────
const mobile = typeof window !== 'undefined' && !!window.Android;
function androidInfo() { try { return mobile ? JSON.parse(Android.info()) : null; } catch { return null; } }
async function openScanned(text) {
  const t = (text || '').trim(); if (!t) return;
  if (/^unishare:/i.test(t)) {
    try { const r = await api('/api/ticket/parse', { method: 'POST', body: { data: t } }); if (r.lan) return openNew('lan', { target: t }); } catch { /* fall through */ }
    return openNew('download', { url: t });
  }
  if (/^https?:\/\//i.test(t)) return openNew('download', { url: t });
  if (/^\d{1,3}(\.\d{1,3}){3}(:\d+)?$/.test(t)) return openNew('lan', { target: t });
  toast('El código no es un ticket ni un link de uni-share', 'warn', 6000);
}
function androidEvent(kind, payload) {
  if (kind === 'toast') return toast(String(payload), 'info');
  if (kind === 'folder') { toast(payload ? `Carpeta de descargas: ${payload}` : 'Se usará la carpeta privada de la app', 'ok'); if (S.tab === 'settings') render(); return; }
  if (kind === 'picked') { const f = pickWaiter; pickWaiter = null; if (f) f(String(payload)); else openNew('lan', { path: String(payload) }); return; }
  if (kind === 'exported') return toast(`«${payload.name}»: ${payload.files} archivo(s) copiados a ${payload.folder}`, 'ok', 6000);
}
function exportToSaf(j) { if (!mobile || !j.saved || !j.saved.length) return; try { Android.exportJob(j.id, JSON.stringify(j.saved), j.name); } catch { /* bridge unavailable */ } }
// Classic script: top-level function declarations (openScanned, androidEvent, openNew) are already window globals for the Kotlin shell.

// ───────────────────────── state & API ─────────────────────────
const S = { data: null, cfg: null, online: false, tab: localStorage.mtab || 'home', stack: [], filter: 'all', q: '', history: null, lastStates: new Map(), lastPending: new Set(), lastNotice: 0, scanning: false };
async function api(path, { method = 'GET', body } = {}) {
  const r = await fetch(path, { method, headers: body ? { 'Content-Type': 'application/json' } : {}, body: body ? JSON.stringify(body) : undefined });
  const txt = await r.text(); let j = null; try { j = txt ? JSON.parse(txt) : null; } catch { j = { raw: txt }; }
  if (!r.ok) throw new Error((j && (j.error || j.message)) || txt || `HTTP ${r.status}`);
  return j;
}
const job = id => (S.data?.jobs || []).find(j => j.id === id);

function applyState(d) {
  const first = !S.data; S.online = true; $('#offline').hidden = true;
  for (const j of d.jobs) {
    const prev = S.lastStates.get(j.id);
    if (prev && prev !== j.state && !first) {
      if (j.state === 'completed') { toast(`«${j.name}» completada`, 'ok'); if (j.kind === 'lan_receive' || j.kind === 'download') exportToSaf(j); }
      else if (j.state === 'failed') toast(`«${j.name}» falló: ${j.message || 'error'}`, 'err', 7000);
    }
    S.lastStates.set(j.id, j.state);
  }
  const pend = new Set(d.pending.map(o => o.transfer_id));
  if (!first) for (const id of pend) if (!S.lastPending.has(id)) { vib([40, 60, 40]); const o = d.pending.find(x => x.transfer_id === id); toast(`${o.sender} quiere enviarte «${o.name}»`, 'warn', 8000, { label: 'Ver', fn: () => { S.stack = []; go('home'); } }); }
  S.lastPending = pend;
  for (const n of d.notices || []) if (n.id > S.lastNotice) { toast(n.text, n.kind === 'danger' ? 'err' : n.kind === 'warning' ? 'warn' : 'info', 8000); S.lastNotice = n.id; }
  if (S.lastNotice) api('/api/notices/ack', { method: 'POST', body: { up_to: S.lastNotice } }).catch(() => {});
  S.data = d;
  const act = d.jobs.filter(isActive).length + d.pending.length; const dot = $('#tab-dot'); dot.hidden = !act; dot.textContent = act > 9 ? '9+' : act;
  render();
}
let es = null, pollT = null;
function connect() {
  if (es) es.close();
  es = new EventSource('/api/events');
  es.addEventListener('state', e => { try { applyState(JSON.parse(e.data)); } catch (err) { console.error(err); } });
  es.onerror = () => { S.online = false; $('#offline').hidden = false; es.close(); es = null; if (!pollT) pollT = setInterval(poll, 2000); };
}
async function poll() { try { applyState(await api('/api/state')); clearInterval(pollT); pollT = null; if (!S.shot) connect(); } catch { S.online = false; $('#offline').hidden = false; } }
async function loadCfg() { try { const r = await api('/api/config'); S.cfg = r.config; S.cfgPath = r.path; if (S.tab === 'settings') render(); } catch { /* engine not up yet */ } }

// ───────────────────────── toasts, clipboard, haptics ─────────────────────────
function toast(msg, type = 'info', ms = 3500, action) {
  const box = $('#toasts'); const t = h('div', { class: `toast ${type}`, role: 'status' }, h('span', { class: 't' }, msg));
  if (action) t.append(h('button', { onclick: () => { action.fn(); t.remove(); } }, action.label));
  box.append(t); while (box.children.length > 3) box.firstChild.remove();
  setTimeout(() => { t.classList.add('out'); setTimeout(() => t.remove(), 200); }, ms);
}
async function copy(text, what = 'Copiado') { try { await navigator.clipboard.writeText(text); toast(what, 'ok', 1800); } catch { const ta = h('textarea', {}, text); document.body.append(ta); ta.select(); try { document.execCommand('copy'); toast(what, 'ok', 1800); } catch { toast('No se pudo copiar', 'err'); } ta.remove(); } }
function vib(p) { try { navigator.vibrate && navigator.vibrate(p); } catch { /* ignore */ } }
function shareText(text, title) { if (mobile) return Android.shareText(text, title || 'uni-share'); if (navigator.share) return navigator.share({ title, text }).catch(() => {}); return copy(text); }

// ───────────────────────── bottom sheets ─────────────────────────
const sheets = [];
function sheet({ title, body, foot, cls = '', onClose, back }) {
  const ov = h('div', { class: 'ov', role: 'dialog', 'aria-modal': 'true', 'aria-label': title });
  const sh = h('div', { class: `sheet ${cls}` });
  sh.append(h('div', { class: 'grab' }));
  const head = h('div', { class: 'sh' });
  if (back) head.append(h('button', { class: 'ib', 'aria-label': 'Volver', html: ico('back'), onclick: back }));
  head.append(h('h2', {}, title), h('button', { class: 'ib', 'aria-label': 'Cerrar', html: ico('x'), onclick: () => ov.close() }));
  const sb = h('div', { class: 'sb' }); if (typeof body === 'string') sb.innerHTML = body; else if (body) sb.append(body);
  sh.append(head, sb); if (foot) { const sf = h('div', { class: 'sf' }); if (typeof foot === 'string') sf.innerHTML = foot; else sf.append(foot); sh.append(sf); }
  ov.append(sh); ov.body = sb; ov.head = head; ov.sheet = sh;
  ov.onclick = e => { if (e.target === ov) ov.close(); };
  let y0 = null, dy = 0;
  sh.addEventListener('touchstart', e => { if (sb.scrollTop > 0 && sb.contains(e.target)) return; y0 = e.touches[0].clientY; dy = 0; sh.style.transition = 'none'; }, { passive: true });
  sh.addEventListener('touchmove', e => { if (y0 == null) return; dy = Math.max(0, e.touches[0].clientY - y0); sh.style.transform = `translateY(${dy}px)`; }, { passive: true });
  sh.addEventListener('touchend', () => { if (y0 == null) return; sh.style.transition = ''; if (dy > 90) ov.close(); else sh.style.transform = ''; y0 = null; });
  ov.close = (silent) => { if (!ov.isConnected) return; ov.classList.add('out'); setTimeout(() => ov.remove(), 150); const i = sheets.indexOf(ov); if (i >= 0) sheets.splice(i, 1); onClose && onClose(); if (!silent) syncBack(); };
  $('#sheets').append(ov); sheets.push(ov); syncBack();
  setTimeout(() => { const f = sb.querySelector('input:not([type=checkbox]),textarea'); if (f && !mobile) f.focus(); }, 80);
  return ov;
}
function confirmSheet(text, okLabel = 'Confirmar', danger = false, title = '¿Seguro?') {
  return new Promise(res => {
    let ok = false;
    const ov = sheet({ title, body: h('p', { class: 'hint', style: 'margin:6px 0 4px;font-size:var(--fs)' }, text), onClose: () => res(ok),
      foot: h('div', { class: 'btns', style: 'margin:0;width:100%' }, h('button', { class: 'btn', onclick: () => ov.close() }, 'Cancelar'), h('button', { class: `btn ${danger ? 'danger' : 'primary'}`, onclick: () => { ok = true; ov.close(); } }, okLabel)) });
  });
}
function menuSheet(title, items) {
  const list = h('div', { class: 'menu' });
  const ov = sheet({ title, body: list });
  for (const it of items) if (it) list.append(h('button', { class: `row${it.danger ? ' danger' : ''}${it.dis ? ' dis' : ''}`, disabled: it.dis, onclick: () => { ov.close(); it.fn(); } },
    h('span', { class: 'ic', html: ico(it.icon) }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, it.label), it.sub ? h('div', { class: 'sub' }, it.sub) : null)));
  return ov;
}
function promptSheet(title, { label, value = '', placeholder = '', type = 'text', sub, okLabel = 'Guardar' } = {}) {
  return new Promise(res => {
    let out = null;
    const inp = h('input', { type, value, placeholder, autocomplete: 'off', enterkeyhint: 'done' });
    const ov = sheet({ title, onClose: () => res(out), body: h('div', { class: 'field' }, label ? h('label', {}, label) : null, inp, sub ? h('div', { class: 'sub' }, sub) : null),
      foot: h('div', { class: 'btns', style: 'margin:0;width:100%' }, h('button', { class: 'btn', onclick: () => ov.close() }, 'Cancelar'), h('button', { class: 'btn primary', onclick: () => { out = inp.value; ov.close(); } }, okLabel)) });
    inp.onkeydown = e => { if (e.key === 'Enter') { out = inp.value; ov.close(); } };
  });
}

// ───────────────────────── back button / router ─────────────────────────
// history depth == sheets.length + stack.length so the hardware back button pops exactly one layer.
let syncing = false;
function depth() { return sheets.length + S.stack.length; }
function syncBack() {
  if (syncing) return;
  const cur = (history.state && history.state.depth) || 0, want = depth();
  if (want > cur) { for (let i = cur; i < want; i++) history.pushState({ depth: i + 1 }, ''); }
  else if (want < cur) { syncing = true; history.go(want - cur); }
}
window.addEventListener('popstate', () => {
  if (syncing) { syncing = false; return; }
  const cur = (history.state && history.state.depth) || 0;
  if (cur < depth()) { if (sheets.length) sheets[sheets.length - 1].close(true); else { S.stack.pop(); render(); } }
  syncBack();
});
function go(tab) { S.tab = tab; localStorage.mtab = tab; S.stack = []; render(); syncBack(); }
function push(view) { S.stack.push(view); render(); syncBack(); }
function pop() { if (S.stack.length) { S.stack.pop(); render(); syncBack(); } }

const TITLES = { home: '', jobs: 'Transferencias', devices: 'Dispositivos', share: 'Compartir', settings: 'Ajustes' };
function render() {
  const page = $('#page'); const top = S.stack[S.stack.length - 1];
  const title = top ? top.title : TITLES[S.tab];
  $('#t-title').textContent = title || ''; $('#t-brand').hidden = !!title; $('#t-back').hidden = !top; $('#t-qr').hidden = !mobile || !!top;
  $$('#tabs button').forEach(b => b.classList.toggle('active', b.dataset.tab === S.tab));
  const fabOn = !top && (S.tab === 'home' || S.tab === 'jobs' || S.tab === 'share'); $('#fab').classList.toggle('hide', !fabOn); page.classList.toggle('no-fab', !fabOn);
  const y = page.scrollTop;
  page.innerHTML = '';
  if (!S.data) { page.append(h('div', { class: 'empty' }, h('span', { html: ico('wifi') }), h('b', {}, 'Conectando con el motor…'), h('p', {}, 'Si tarda, comprueba que uni-share está en ejecución.'))); return; }
  page.append(top ? renderStack(top) : SCREENS[S.tab]());
  wire(page, top);
  if (top && top.keepScroll) page.scrollTop = y;
}
function renderStack(v) {
  if (v.kind === 'job') return viewJob(v.id);
  if (v.kind === 'history') return viewHistory();
  if (v.kind === 'security') return viewSecurity();
  if (v.kind === 'about') return viewAbout();
  return h('div');
}

// ───────────────────────── partials ─────────────────────────
function jobRow(j) {
  const [ic, dir] = KICON[j.kind] || ['file', ''];
  const act = isActive(j); const p = pct(j);
  const sub = act ? `${fmtB(j.done)} / ${fmtB(j.total)}${j.speed ? ' · ' + fmtS(j.speed) : ''}${j.eta != null ? ' · ' + fmtT(j.eta) : ''}` : `${KIND[j.kind] || j.kind} · ${j.peer || j.link || ''} · ${fmtAgo(j.finished || j.started)}`;
  const end = act ? h('span', { class: 'pct' }, (j.state === 'queued' ? 'cola' : p + '%')) : h('span', { class: `badge ${j.state}` }, STATE[j.state] || j.state);
  const row = h('button', { class: 'row', 'data-job': j.id, 'aria-label': `${j.name}, ${STATE[j.state]}` },
    h('span', { class: `ic ${j.state === 'failed' ? 'err' : j.state === 'completed' ? 'ok' : dir}`, html: ico(j.state === 'failed' ? 'x' : j.state === 'completed' ? 'check' : ic) }),
    h('span', { class: 'body' }, h('div', { class: 'ttl' }, j.name), h('div', { class: 'sub' }, sub), act ? h('div', { class: `bar ${j.state}${j.total ? '' : ' indet'}` }, h('i', { style: `width:${p}%` })) : null),
    h('span', { class: 'end' }, end, j.scan && j.scan !== 'info' ? h('span', { class: `badge ${SCAN[j.scan][0]}`, html: ico('shield') + SCAN[j.scan][1] }) : null),
    h('span', { class: 'chev', html: ico('chev') }));
  return row;
}
function offerCard(o) {
  const files = o.files.length; const resume = o.resume_bytes ? ` · reanuda ${fmtB(o.resume_bytes)}` : '';
  return h('div', { class: 'offer', 'data-offer': o.transfer_id },
    h('div', { class: 'h' }, h('span', { class: 'ic', html: ico('inbox') }), h('div', { class: 'body' }, h('b', {}, `${o.sender} quiere enviarte «${o.name}»`),
      h('div', { class: 'meta' }, `${files} archivo${files === 1 ? '' : 's'} · ${fmtB(o.total_size)}${o.compressed ? ' · comprimido' : ''}${resume}`),
      h('code', {}, `${o.peer} · huella ${o.sender_fingerprint}`))),
    h('div', { class: 'btns' }, h('button', { class: 'btn', 'data-act': 'reject', html: ico('x') + 'Rechazar' }), h('button', { class: 'btn primary', 'data-act': 'accept', html: ico('check') + 'Aceptar' })));
}
function empty(icon, title, text, action) {
  return h('div', { class: 'empty' }, h('span', { html: ico(icon) }), h('b', {}, title), h('p', {}, text), action ? h('button', { class: 'btn primary', style: 'margin-top:10px', 'data-empty-act': action[1], html: ico(action[2] || 'plus') + action[0] }) : null);
}
function wireCommon(root) {
  $$('[data-job]', root).forEach(el => el.onclick = () => { const id = Number(el.dataset.job); const j = job(id); if (j) push({ kind: 'job', id, title: KIND[j.kind] || 'Transferencia' }); });
  $$('[data-offer]', root).forEach(card => {
    const id = card.dataset.offer;
    $$('[data-act]', card).forEach(b => b.onclick = async e => { e.stopPropagation(); b.disabled = true; try { if (b.dataset.act === 'accept') { const r = await api(`/api/offers/${encodeURIComponent(id)}/accept`, { method: 'POST', body: {} }); toast('Recibiendo…', 'ok'); if (r.job) push({ kind: 'job', id: r.job.id, title: 'Recepción LAN' }); } else { await api(`/api/offers/${encodeURIComponent(id)}/reject`, { method: 'POST' }); toast('Rechazada', 'info'); } } catch (err) { toast(err.message, 'err'); b.disabled = false; } });
  });
  $$('[data-empty-act]', root).forEach(b => b.onclick = () => { const a = b.dataset.emptyAct; if (a === 'scan') return mobile ? Android.scanQr() : openNew('download'); if (a === 'qr') return go('share'); openNew(a === 'new' ? 'lan' : a); });
  $$('[data-go]', root).forEach(b => b.onclick = () => go(b.dataset.go));
  $$('[data-push]', root).forEach(b => b.onclick = () => push({ kind: b.dataset.push, title: b.dataset.title || { history: 'Historial', security: 'Seguridad', about: 'Acerca de' }[b.dataset.push] }));
  $$('[data-copy]', root).forEach(b => b.onclick = () => copy(b.dataset.copy, b.dataset.what || 'Copiado'));
}

// ───────────────────────── screens ─────────────────────────
const SCREENS = { home: viewHome, jobs: viewJobs, devices: viewDevices, share: viewShare, settings: viewSettings };

function viewHome() {
  const d = S.data; const act = d.jobs.filter(isActive); const recent = d.jobs.filter(j => !isActive(j)).sort((a, b) => (b.finished || 0) - (a.finished || 0)).slice(0, 5);
  const online = !!d.lan_addr;
  const root = h('div');
  root.append(h('div', { class: 'hero' }, h('span', { class: 'av', html: ico('logo') }),
    h('div', { class: 'body' }, h('b', {}, d.device_name), h('div', { class: 'st' }, h('i', { class: online ? '' : 'off' }), online ? `Visible en la red · ${d.lan_addr}` : 'Sin red local'), h('code', {}, `huella ${d.fingerprint}${d.pin_required ? ' · PIN' : ''}`)),
    h('div', { class: 'speeds' }, h('span', { class: 'd', html: ico('down') + fmtS(d.speed_down) }), h('span', { class: 'u', html: ico('up') + fmtS(d.speed_up) }))));
  root.append(h('div', { class: 'quick' },
    qa('lan', 'send', 'Enviar LAN'), qa('receive', 'inbox', 'Recibir', 'amber'), qa('qr', 'qr', mobile ? 'Escanear' : 'Mi QR'), qa('download', 'dl', 'Descargar')));
  if (d.pending.length) { root.append(h('div', { class: 'sec' }, `Solicitudes (${d.pending.length})`)); d.pending.forEach(o => root.append(offerCard(o))); }
  if (act.length) { root.append(h('div', { class: 'sec' }, `En curso (${act.length})`, h('span', { class: 'sp' }), h('button', { 'data-go': 'jobs', html: 'Ver todas ' + ico('chev') })), h('div', { class: 'list' }, act.map(jobRow))); }
  if (recent.length) { root.append(h('div', { class: 'sec' }, 'Recientes', h('span', { class: 'sp' }), h('button', { 'data-push': 'history', html: 'Historial ' + ico('chev') })), h('div', { class: 'list' }, recent.map(jobRow))); }
  if (!d.pending.length && !act.length && !recent.length) root.append(empty('logo', 'Todo listo', 'Envía archivos a otro dispositivo de la red, comparte un link o descarga un ticket.', ['Nueva transferencia', 'new']));
  return root;
}
function qa(q, icon, label, cls = '') { return h('button', { class: 'qa', 'data-q': q }, h('span', { class: `ic ${cls}`, html: ico(icon) }), label); }
function wireHome(root) {
  $$('[data-q]', root).forEach(b => b.onclick = () => {
    const q = b.dataset.q;
    if (q === 'lan') return openNew('lan'); if (q === 'download') return openNew('download');
    if (q === 'qr') return mobile ? Android.scanQr() : go('share');
    if (q === 'receive') return receiveSheet();
  });
}
function receiveSheet() {
  const d = S.data;
  const ov = sheet({ title: 'Recibir en este dispositivo', body: h('div', {},
    h('p', { class: 'hint', style: 'margin:4px 0 12px' }, 'El otro dispositivo puede escanear este código o elegir tu nombre en su lista LAN.'),
    d.pairing_uri ? h('div', { class: 'qrbox' }, h('img', { src: '/api/qr?data=' + encodeURIComponent(d.pairing_uri), alt: 'QR de emparejamiento' })) : h('div', { class: 'alert warn', html: ico('info') + '<span>Sin dirección LAN: conecta a una red Wi‑Fi.</span>' }),
    h('dl', { class: 'kv' }, h('dt', {}, 'Nombre'), h('dd', {}, d.device_name), h('dt', {}, 'Dirección'), h('dd', { class: 'mono' }, d.lan_addr || '—'), h('dt', {}, 'Huella'), h('dd', { class: 'mono' }, d.fingerprint), h('dt', {}, 'PIN'), h('dd', {}, d.pin_required ? 'requerido' : 'no'), h('dt', {}, 'Auto‑aceptar'), h('dd', {}, d.auto_accept ? 'sí' : 'no')),
    h('div', { class: 'btns' }, h('button', { class: 'btn', html: ico('copy') + 'Copiar ticket', onclick: () => copy(d.pairing_uri, 'Ticket de emparejamiento copiado') }), h('button', { class: 'btn primary', html: ico('share') + 'Compartir', onclick: () => shareText(d.pairing_uri, 'Emparejar con ' + d.device_name) }))) });
  return ov;
}

const FILTERS = [['all', 'Todas', () => true], ['active', 'Activas', isActive], ['receiving', 'Recibiendo', j => j.kind === 'lan_receive'], ['sending', 'Enviando', j => j.kind === 'lan_send'], ['links', 'Links', j => j.kind === 'global_upload'], ['downloads', 'Descargas', j => j.kind === 'download'], ['done', 'Completadas', j => j.state === 'completed'], ['failed', 'Fallidas', j => j.state === 'failed' || j.state === 'cancelled']];
function viewJobs() {
  const d = S.data; const root = h('div');
  const q = S.q.trim().toLowerCase();
  const f = FILTERS.find(x => x[0] === S.filter) || FILTERS[0];
  const list = d.jobs.filter(f[2]).filter(j => !q || j.name.toLowerCase().includes(q) || (j.peer || '').toLowerCase().includes(q) || (j.link || '').toLowerCase().includes(q)).sort((a, b) => (isActive(b) - isActive(a)) || (b.started - a.started));
  root.append(h('div', { class: 'search' }, h('span', { html: ico('search') }), h('input', { id: 'q', type: 'search', placeholder: 'Buscar por nombre, equipo o link…', value: S.q, enterkeyhint: 'search' })));
  root.append(h('div', { class: 'chips' }, FILTERS.map(([id, label, fn]) => { const n = d.jobs.filter(fn).length; return h('button', { class: `chip${S.filter === id ? ' active' : ''}`, 'data-f': id }, label, n ? h('span', { class: 'n' }, n) : null); })));
  if (d.pending.length && (S.filter === 'all' || S.filter === 'active' || S.filter === 'receiving')) d.pending.forEach(o => root.append(offerCard(o)));
  if (list.length) root.append(h('div', { class: 'list' }, list.map(jobRow)));
  else root.append(empty(q ? 'search' : 'list', q ? 'Sin resultados' : 'No hay transferencias', q ? `Nada coincide con «${S.q}».` : 'Cuando envíes o recibas algo aparecerá aquí.', q ? null : ['Nueva transferencia', 'new']));
  const done = d.jobs.filter(j => !isActive(j)).length;
  if (done) root.append(h('div', { class: 'btns' }, h('button', { class: 'btn ghost', id: 'clear-done', html: ico('trash') + `Quitar ${done} terminada${done === 1 ? '' : 's'}` })));
  return root;
}
function wireJobs(root) {
  const q = $('#q', root); if (q) { q.oninput = () => { S.q = q.value; const pos = q.selectionStart; render(); const nq = $('#q'); nq.focus(); try { nq.setSelectionRange(pos, pos); } catch { /* search inputs on some browsers */ } }; }
  $$('[data-f]', root).forEach(b => b.onclick = () => { S.filter = b.dataset.f; render(); });
  const c = $('#clear-done', root); if (c) c.onclick = async () => { if (await confirmSheet('Se quitan de la lista las transferencias terminadas (el historial se conserva).', 'Quitar')) { await api('/api/jobs/clear-finished', { method: 'POST' }).catch(e => toast(e.message, 'err')); } };
}

function viewJob(id) {
  const j = job(id); const root = h('div');
  if (!j) { root.append(empty('x', 'Transferencia no encontrada', 'Puede que se haya quitado de la lista.')); return root; }
  const [ic, dir] = KICON[j.kind] || ['file', '']; const act = isActive(j); const p = pct(j);
  root.append(h('div', { class: 'dhead' }, h('span', { class: `ic ${dir}`, html: ico(ic) }), h('div', { class: 'body' }, h('b', {}, j.name), h('div', { class: 'm' }, `${KIND[j.kind]} · ${j.peer || ''}`)),
    h('button', { class: 'ib', 'aria-label': 'Más', html: ico('more'), onclick: () => jobMenu(j) })));
  root.append(h('div', { style: 'display:flex;gap:6px;flex-wrap:wrap;margin-bottom:12px' }, h('span', { class: `badge ${j.state}` }, STATE[j.state]), j.scan ? h('span', { class: `badge ${SCAN[j.scan][0]}`, html: ico('shield') + ' ' + SCAN[j.scan][1] }) : null, j.retryable ? h('span', { class: 'badge queued' }, 'reintentable') : null));
  root.append(h('div', { class: 'card bigprog' }, h('div', { class: `bar ${j.state}${act && !j.total ? ' indet' : ''}` }, h('i', { style: `width:${p}%` })),
    h('div', { class: 'nums' }, h('span', {}, `${fmtB(j.done)} / ${fmtB(j.total)}`), h('span', {}, act ? `${p}% · ${fmtS(j.speed)}${j.eta != null ? ' · ' + fmtT(j.eta) : ''}` : `${p}%`)),
    j.current_file && act ? h('div', { class: 'hint trunc', style: 'margin-top:8px' }, j.current_file) : null,
    j.message ? h('div', { class: `alert ${j.state === 'failed' ? 'err' : ''}`, style: 'margin:10px 0 0', html: ico(j.state === 'failed' ? 'x' : 'info') + `<span>${esc(j.message)}</span>` }) : null));
  const btns = h('div', { class: 'btns', style: 'margin:0 0 12px' });
  if (act) btns.append(h('button', { class: 'btn danger', 'data-a': 'cancel', html: ico('stop') + 'Cancelar' }));
  if (j.link || j.ticket_uri) btns.append(h('button', { class: 'btn primary', 'data-a': 'share', html: ico('share') + 'Compartir' }));
  if (!act && j.retryable) btns.append(h('button', { class: 'btn primary', 'data-a': 'retry', html: ico('refresh') + 'Reintentar' }));
  if (!act && (j.kind === 'lan_receive' || j.kind === 'download') && j.state === 'completed') btns.append(h('button', { class: 'btn', 'data-a': 'rescan', html: ico('shield') + 'Analizar' }));
  if (!act) btns.append(h('button', { class: 'btn ghost', 'data-a': 'remove', html: ico('trash') + 'Quitar' }));
  if (btns.children.length) root.append(btns);
  const kv = h('dl', { class: 'kv' });
  const add = (k, v, mono) => { if (v) kv.append(h('dt', {}, k), h('dd', { class: mono ? 'mono' : '' }, v)); };
  add('Equipo / origen', j.peer); add('Link', j.link, true); add('Destino', j.dest, true); add('Ticket', j.ticket_path, true);
  add('Inicio', fmtDate(j.started)); add('Fin', j.finished ? fmtDate(j.finished) : null); add('Origen', j.origin === 'cli' ? 'línea de comandos' : j.origin === 'tray' ? 'bandeja' : null); add('ID', '#' + j.id, true);
  root.append(h('div', { class: 'card' }, h('h3', {}, 'Detalles'), kv));
  if (j.files && j.files.length) {
    const files = h('div', { class: 'list files' }); const shown = j.files.slice(0, 50);
    shown.forEach(f => files.append(h('div', { class: 'row' }, h('span', { class: 'ic', html: ico('file') }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, f.path)), h('span', { class: 'end' }, fmtB(f.size)))));
    if (j.files.length > shown.length) files.append(h('div', { class: 'row hint' }, `… y ${j.files.length - shown.length} más`));
    root.append(h('div', { class: 'sec' }, `Archivos (${j.files.length})`), files);
  }
  if (j.log && j.log.length) root.append(h('div', { class: 'sec' }, 'Registro'), h('pre', { class: 'log' }, j.log.slice(-60).join('\n')));
  return root;
}
function wireJob(root, id) {
  const j = job(id); if (!j) return;
  $$('[data-a]', root).forEach(b => b.onclick = () => ({ cancel: cancelJob, share: openShareSheet, retry: retryJob, rescan: rescanJob, remove: removeJob })[b.dataset.a](j));
}

// ── devices ──
async function rescanDevices() {
  if (S.scanning) return; S.scanning = true; render();
  try { await api('/api/devices'); } catch (e) { toast(e.message, 'err'); }
  setTimeout(() => { S.scanning = false; render(); }, 1200);
}
function viewDevices() {
  const d = S.data; const root = h('div'); const devs = d.devices || [];
  root.append(h('div', { class: `radar${S.scanning ? ' scanning' : ''}` }, h('i'), h('i'), h('i'), h('span', { class: 'me', html: ico('logo') })));
  root.append(h('p', { class: 'hint', style: 'text-align:center;margin:0 0 12px' }, devs.length ? `${devs.length} dispositivo${devs.length === 1 ? '' : 's'} en la red` : (S.scanning ? 'Buscando…' : 'No se ha encontrado ningún dispositivo'), d.devices_scanned_ago != null && !S.scanning ? ` · hace ${fmtT(d.devices_scanned_ago)}` : ''));
  root.append(h('div', { class: 'btns', style: 'margin:0 0 14px' }, h('button', { class: 'btn', id: 'dev-scan', disabled: S.scanning, html: ico('refresh') + (S.scanning ? 'Buscando…' : 'Buscar de nuevo') }), h('button', { class: 'btn primary', id: 'dev-pair', html: ico('qr') + (mobile ? 'Escanear QR' : 'Mi QR') })));
  if (devs.length) root.append(h('div', { class: 'list' }, devs.map(dv => h('button', { class: `row${dv.online ? '' : ' dis'}`, 'data-dev': dv.name },
    h('span', { class: 'ic', html: ico('devices') }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, dv.name), h('div', { class: 'sub' }, h('span', { class: 'mono' }, `${dv.addresses[0] || '?'}:${dv.port}`), ` · ${dv.fingerprint}${dv.version ? ' · v' + dv.version : ''}`)),
    h('span', { class: 'end' }, dv.requires_pin ? h('span', { class: 'badge pin', html: ico('lock') + 'PIN' }) : null, dv.online ? null : h('span', { class: 'badge cancelled' }, 'offline')), h('span', { class: 'chev', html: ico('chev') })))));
  else if (!S.scanning) root.append(empty('devices', 'Nadie por aquí', 'Abre uni-share en otro dispositivo de la misma Wi‑Fi, o escanea su QR para emparejar directamente.'));
  root.append(h('div', { class: 'card' }, h('h3', {}, 'Este dispositivo'), h('dl', { class: 'kv' }, h('dt', {}, 'Nombre'), h('dd', {}, d.device_name), h('dt', {}, 'Dirección'), h('dd', { class: 'mono' }, d.lan_addr || '—'), h('dt', {}, 'IPs'), h('dd', { class: 'mono' }, (d.local_ips || []).join(', ') || '—'), h('dt', {}, 'Huella'), h('dd', { class: 'mono' }, d.fingerprint))));
  return root;
}
function wireDevices(root) {
  $('#dev-scan', root).onclick = rescanDevices;
  $('#dev-pair', root).onclick = () => mobile ? Android.scanQr() : receiveSheet();
  $$('[data-dev]', root).forEach(b => b.onclick = () => { const dv = (S.data.devices || []).find(x => x.name === b.dataset.dev); if (!dv) return; menuSheet(dv.name, [
    { icon: 'send', label: 'Enviar archivos', sub: `${dv.addresses[0]}:${dv.port}${dv.requires_pin ? ' · pide PIN' : ''}`, fn: () => openNew('lan', { device: dv.name }) },
    { icon: 'copy', label: 'Copiar dirección', fn: () => copy(`${dv.addresses[0]}:${dv.port}`, 'Dirección copiada') },
    { icon: 'shield', label: 'Copiar huella', sub: dv.fingerprint, fn: () => copy(dv.fingerprint, 'Huella copiada') }]); });
}

// ── share ──
function viewShare() {
  const d = S.data; const root = h('div');
  root.append(h('div', { class: 'card' }, h('h3', {}, 'Recibir en este dispositivo'),
    d.pairing_uri ? h('div', { class: 'qrbox', id: 'pair-qr' }, h('img', { src: '/api/qr?data=' + encodeURIComponent(d.pairing_uri), alt: 'QR de emparejamiento' })) : h('div', { class: 'alert warn', html: ico('info') + '<span>Sin dirección LAN: conecta a una red Wi‑Fi para que otros puedan enviarte archivos.</span>' }),
    h('p', { class: 'hint', style: 'text-align:center;margin:0 0 10px' }, `${d.device_name} · ${d.lan_addr || '—'} · huella ${d.fingerprint}`),
    h('div', { class: 'btns', style: 'margin:0' }, h('button', { class: 'btn', id: 'pair-copy', disabled: !d.pairing_uri, html: ico('copy') + 'Copiar' }), h('button', { class: 'btn primary', id: 'pair-share', disabled: !d.pairing_uri, html: ico('share') + 'Compartir' }))));
  root.append(h('div', { class: 'sec' }, 'Compartir con cualquiera'));
  root.append(h('div', { class: 'list' },
    shareRow('global', 'globe', 'Subir y crear link', 'Sube a la nube y comparte un link o un ticket .unishare'),
    shareRow('ticket', 'ticket', 'Crear ticket desde links', 'Agrupa links existentes en un ticket firmado'),
    shareRow('lan', 'send', 'Enviar por la red local', 'Directo a otro dispositivo, sin pasar por internet')));
  const links = d.jobs.filter(j => j.kind === 'global_upload' && j.state === 'completed' && (j.link || j.ticket_uri)).slice(0, 8);
  if (links.length) { root.append(h('div', { class: 'sec' }, 'Links recientes')); root.append(h('div', { class: 'list' }, links.map(j => h('button', { class: 'row', 'data-share': j.id }, h('span', { class: 'ic up', html: ico('link') }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, j.name), h('div', { class: 'sub' }, `${fmtB(j.total)} · ${fmtAgo(j.finished)}${j.ticket_uri ? ' · ticket' : ''}`)), h('span', { class: 'chev', html: ico('share') }))))); }
  return root;
}
function shareRow(mode, icon, ttl, sub) { return h('button', { class: 'row', 'data-mode': mode }, h('span', { class: 'ic', html: ico(icon) }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, ttl), h('div', { class: 'sub' }, sub)), h('span', { class: 'chev', html: ico('chev') })); }
function wireShare(root) {
  const d = S.data;
  $('#pair-copy', root).onclick = () => copy(d.pairing_uri, 'Ticket de emparejamiento copiado');
  $('#pair-share', root).onclick = () => shareText(d.pairing_uri, 'Emparejar con ' + d.device_name);
  const qr = $('#pair-qr', root); if (qr) qr.onclick = () => bigQr(d.pairing_uri, `Emparejar con ${d.device_name}`);
  $$('[data-mode]', root).forEach(b => b.onclick = () => openNew(b.dataset.mode));
  $$('[data-share]', root).forEach(b => b.onclick = () => { const j = job(Number(b.dataset.share)); if (j) openShareSheet(j); });
}

// ── settings ──
function viewSettings() {
  const d = S.data; const c = S.cfg; const root = h('div'); const ai = androidInfo();
  if (!c) { root.append(empty('gear', 'Cargando ajustes…', '')); loadCfg(); return root; }
  const sw = (id, icon, ttl, sub, on) => h('button', { class: 'sw', 'data-sw': id, role: 'switch', 'aria-checked': on ? 'true' : 'false' }, h('span', { class: 'ic', html: ico(icon) }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, ttl), sub ? h('div', { class: 'sub' }, sub) : null), h('span', { class: `tg${on ? ' on' : ''}` }));
  const val = (id, icon, ttl, v, sub) => h('button', { class: 'sw', 'data-edit': id }, h('span', { class: 'ic', html: ico(icon) }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, ttl), sub ? h('div', { class: 'sub' }, sub) : null), h('span', { class: 'val' }, v), h('span', { class: 'chev', html: ico('chev') }));
  const nav = (id, icon, ttl, sub) => h('button', { class: 'sw', 'data-push': id }, h('span', { class: 'ic', html: ico(icon) }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, ttl), sub ? h('div', { class: 'sub' }, sub) : null), h('span', { class: 'chev', html: ico('chev') }));
  root.append(h('div', { class: 'sec' }, 'Dispositivo'), h('div', { class: 'list' },
    val('device_name', 'devices', 'Nombre visible', c.device_name),
    mobile ? h('button', { class: 'sw', id: 's-saf' }, h('span', { class: 'ic', html: ico('folder') }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, 'Carpeta de descargas'), h('div', { class: 'sub' }, 'Los archivos recibidos se copian ahí al terminar')), h('span', { class: 'val' }, (ai && ai.downloadTree) || 'privada de la app'), h('span', { class: 'chev', html: ico('chev') }))
      : val('download_dir', 'folder', 'Carpeta de descargas', String(c.download_dir)),
    val('rate_limit_mbps', 'up', 'Límite de velocidad', c.rate_limit_mbps ? c.rate_limit_mbps + ' Mbps' : 'sin límite'),
    h('button', { class: 'sw', id: 's-theme' }, h('span', { class: 'ic', html: ico(document.documentElement.dataset.theme === 'light' ? 'sun' : 'moon') }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, 'Tema'), h('div', { class: 'sub' }, 'Solo afecta a esta pantalla')), h('span', { class: 'val' }, document.documentElement.dataset.theme === 'light' ? 'claro' : 'oscuro'), h('span', { class: 'chev', html: ico('chev') }))));
  root.append(h('div', { class: 'sec' }, 'Recepción'), h('div', { class: 'list' },
    sw('auto_accept', 'inbox', 'Aceptar automáticamente', 'Sin preguntar, de cualquier dispositivo', c.auto_accept),
    val('pin', 'lock', 'PIN de recepción', c.pin ? '••••' : 'sin PIN', 'Quien envíe deberá introducirlo'),
    sw('notifications', 'info', 'Notificaciones', 'Avisos del sistema al terminar o al recibir', c.notifications),
    sw('compress_folders', 'file', 'Comprimir carpetas', 'Empaqueta carpetas antes de enviar', c.compress_folders)));
  root.append(h('div', { class: 'sec' }, 'Links y tickets'), h('div', { class: 'list' },
    val('backend', 'globe', 'Servicio de subida', c.global.backend),
    val('expiry_days', 'history', 'Caducidad por defecto', c.global.expiry_days + ' días'),
    val('parallel_parts', 'ul', 'Partes en paralelo', String(c.global.parallel_parts)),
    sw('sign_tickets', 'shield', 'Firmar tickets', `Ed25519 · ${d.signer_fingerprint}`, c.sign_tickets)));
  root.append(h('div', { class: 'sec' }, 'Más'), h('div', { class: 'list' },
    nav('security', 'shield', 'Seguridad y análisis', d.scan_enabled ? `Análisis activado${d.clamav ? ' · ' + d.clamav : ''}` : 'Análisis desactivado'),
    nav('history', 'history', 'Historial', 'Todas las transferencias registradas'),
    nav('about', 'info', 'Acerca de uni-share', `v${d.version}${ai ? ' · app ' + ai.version : ''}`)));
  root.append(h('p', { class: 'hint', style: 'text-align:center;margin:6px 0 0;font-family:var(--mono);font-size:var(--fs-xs)' }, S.cfgPath || ''));
  return root;
}
async function patch(body) { try { const r = await api('/api/config', { method: 'PUT', body }); S.cfg = r.config || (await api('/api/config')).config; render(); toast('Guardado', 'ok', 1500); } catch (e) { toast(e.message, 'err'); } }
function toggleSetting(id) { patch({ [id]: !S.cfg[id] }); }
async function editSetting(id) {
  const c = S.cfg;
  if (id === 'device_name') { const v = await promptSheet('Nombre visible', { label: 'Así te verán los demás dispositivos', value: c.device_name }); if (v != null && v.trim()) patch({ device_name: v.trim() }); }
  else if (id === 'download_dir') { const p = await pickPath({ dirsOnly: true, start: String(c.download_dir), title: 'Carpeta de descargas' }); if (p) patch({ download_dir: p }); }
  else if (id === 'rate_limit_mbps') { const v = await promptSheet('Límite de velocidad', { label: 'Mbps (0 = sin límite)', value: c.rate_limit_mbps || 0, type: 'number' }); if (v != null) patch({ rate_limit_mbps: Math.max(0, Number(v) || 0) }); }
  else if (id === 'pin') { const v = await promptSheet('PIN de recepción', { label: 'Deja vacío para quitarlo', value: '', type: 'password', sub: 'Quien te envíe tendrá que escribirlo' }); if (v != null) patch({ pin: v }); }
  else if (id === 'expiry_days') { const v = await promptSheet('Caducidad de los links', { label: 'Días', value: c.global.expiry_days, type: 'number' }); if (v != null) patch({ expiry_days: Math.max(1, Number(v) || 7) }); }
  else if (id === 'parallel_parts') { const v = await promptSheet('Partes en paralelo', { label: 'Conexiones simultáneas por subida', value: c.global.parallel_parts, type: 'number' }); if (v != null) patch({ parallel_parts: Math.min(16, Math.max(1, Number(v) || 4)) }); }
  else if (id === 'backend') toast('El servicio de subida se cambia en config.toml ([global].backend)', 'info', 5000);
}
function wireSettings(root) {
  $$('[data-sw]', root).forEach(b => b.onclick = () => toggleSetting(b.dataset.sw));
  $$('[data-edit]', root).forEach(b => b.onclick = () => editSetting(b.dataset.edit));
  const saf = $('#s-saf', root); if (saf) saf.onclick = () => menuSheet('Carpeta de descargas', [{ icon: 'folder', label: 'Elegir carpeta…', sub: 'Selector del sistema (SAF)', fn: () => Android.pickDownloadFolder() }, { icon: 'x', label: 'Usar la carpeta privada de la app', fn: () => { Android.clearDownloadFolder(); render(); } }]);
  $('#s-theme', root).onclick = toggleTheme;
}

// ── security ──
function viewSecurity() {
  const d = S.data; const c = S.cfg || {}; const root = h('div');
  root.append(h('div', { class: 'alert', html: ico('shield') + '<span>Todo lo recibido se analiza antes de darlo por bueno: tipo real del archivo, extensiones dobles, ejecutables, scripts, archivos con macros y, si está instalado, ClamAV.</span>' }));
  const sw = (id, ttl, sub, on) => h('button', { class: 'sw', 'data-sw': id, role: 'switch', 'aria-checked': on ? 'true' : 'false' }, h('span', { class: 'body' }, h('div', { class: 'ttl' }, ttl), sub ? h('div', { class: 'sub' }, sub) : null), h('span', { class: `tg${on ? ' on' : ''}` }));
  root.append(h('div', { class: 'sec' }, 'Análisis'), h('div', { class: 'list' },
    sw('scan_enabled', 'Analizar lo recibido', 'Recepciones LAN y descargas', d.scan_enabled),
    sw('scan_clamav', 'Usar ClamAV', d.clamav ? `Detectado: ${d.clamav}` : 'No se ha encontrado clamscan/clamdscan', d.scan_clamav),
    h('div', { class: 'sw' }, h('span', { class: 'body' }, h('div', { class: 'ttl' }, 'Si hay peligro'), h('div', { class: 'sub' }, 'Qué hacer con un archivo marcado como peligroso')),
      h('select', { id: 's-danger', style: 'height:36px;border-radius:6px;background:var(--bg-3);color:var(--fg);border:1px solid var(--line);padding:0 8px' }, ...[['report', 'Solo avisar'], ['quarantine', 'Cuarentena'], ['delete', 'Eliminar']].map(([v, l]) => h('option', { value: v, selected: d.scan_on_danger === v }, l))))));
  root.append(h('div', { class: 'sec' }, 'Identidad'), h('div', { class: 'card' }, h('dl', { class: 'kv' },
    h('dt', {}, 'Huella LAN'), h('dd', { class: 'mono' }, d.fingerprint_full || d.fingerprint), h('dt', {}, 'Firma tickets'), h('dd', { class: 'mono' }, d.signer_fingerprint), h('dt', {}, 'Firmar'), h('dd', {}, d.sign_tickets ? 'sí' : 'no'), h('dt', {}, 'PIN'), h('dd', {}, c.pin ? 'activado' : 'no')),
    h('div', { class: 'btns' }, h('button', { class: 'btn', 'data-copy': d.fingerprint_full || d.fingerprint, 'data-what': 'Huella copiada', html: ico('copy') + 'Copiar huella' }), h('button', { class: 'btn', 'data-copy': d.signer_fingerprint, 'data-what': 'Huella de firma copiada', html: ico('copy') + 'Copiar firma' }))));
  root.append(h('p', { class: 'hint', style: 'margin:8px 4px' }, 'Comprueba la huella con la otra persona por otro canal (llamada, mensaje) antes de aceptar archivos de un dispositivo desconocido.'));
  return root;
}
function wireSecurity(root) {
  $$('[data-sw]', root).forEach(b => b.onclick = () => toggleSetting(b.dataset.sw));
  $('#s-danger', root).onchange = e => patch({ scan_on_danger: e.target.value });
}

// ── history ──
async function loadHistory() { try { S.history = await api('/api/history'); } catch (e) { S.history = []; toast(e.message, 'err'); } render(); }
function viewHistory() {
  const root = h('div');
  if (S.history == null) { loadHistory(); root.append(empty('history', 'Cargando…', '')); return root; }
  const recs = S.history.slice().sort((a, b) => (b.timestamp || 0) - (a.timestamp || 0));
  if (!recs.length) { root.append(empty('history', 'Historial vacío', 'Aquí quedará constancia de todo lo enviado, recibido y descargado.')); return root; }
  const KI = { lan_send: ['send', 'up'], lan_receive: ['inbox', 'down'], global_upload: ['ul', 'up'], download: ['dl', 'down'] };
  const ST = { in_progress: ['running', 'En curso'], completed: ['completed', 'Completada'], failed: ['failed', 'Fallida'], rejected: ['cancelled', 'Rechazada'] };
  let day = '';
  const list = h('div');
  for (const r of recs) {
    const dd = new Date(r.timestamp).toLocaleDateString('es', { weekday: 'long', day: 'numeric', month: 'long' });
    if (dd !== day) { day = dd; list.append(h('div', { class: 'sec' }, dd)); list.append(h('div', { class: 'list' })); }
    const [ic, dir] = KI[r.kind] || ['file', '']; const st = ST[r.status] || ['queued', r.status];
    list.lastChild.append(h('button', { class: 'row', 'data-hist': r.id }, h('span', { class: `ic ${dir}`, html: ico(ic) }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, r.name), h('div', { class: 'sub' }, `${KIND[r.kind] || r.kind} · ${r.peer_or_link || ''} · ${fmtB(r.size)}${r.file_count > 1 ? ' · ' + r.file_count + ' archivos' : ''}`)), h('span', { class: 'end' }, h('span', { class: `badge ${st[0]}` }, st[1]), h('span', {}, new Date(r.timestamp).toLocaleTimeString('es', { hour: '2-digit', minute: '2-digit' })))));
  }
  root.append(list);
  root.append(h('div', { class: 'btns' }, h('button', { class: 'btn ghost', id: 'hist-clear', html: ico('trash') + 'Borrar historial' })));
  return root;
}
function wireHistory(root) {
  const hc = $('#hist-clear', root); if (hc) hc.onclick = async () => { if (await confirmSheet('Se borra el registro de transferencias. Los archivos no se tocan.', 'Borrar', true)) { await api('/api/history', { method: 'DELETE' }).catch(e => toast(e.message, 'err')); S.history = null; render(); } };
  $$('[data-hist]', root).forEach(b => b.onclick = () => { const r = S.history.find(x => x.id === b.dataset.hist || String(x.id) === b.dataset.hist); if (!r) return; const items = [];
    if (r.peer_or_link && /^https?:|^unishare:/.test(r.peer_or_link)) { items.push({ icon: 'copy', label: 'Copiar link', sub: r.peer_or_link, fn: () => copy(r.peer_or_link, 'Link copiado') }); items.push({ icon: 'dl', label: 'Descargar de nuevo', fn: () => openNew('download', { url: r.peer_or_link }) }); }
    if (r.error) items.push({ icon: 'info', label: 'Error', sub: r.error, fn: () => copy(r.error, 'Error copiado') });
    items.push({ icon: 'info', label: fmtDate(r.timestamp), sub: `${r.kind} · ${r.status}${r.meta ? ' · ' + JSON.stringify(r.meta).slice(0, 80) : ''}`, fn: () => {} });
    menuSheet(r.name, items); });
}

// ── about ──
function viewAbout() {
  const d = S.data; const ai = androidInfo(); const root = h('div');
  root.append(h('div', { class: 'about' }, h('span', { class: 'logo', html: ico('logo') }), h('b', {}, 'uni-share'), h('div', {}, `versión ${d.version}${ai ? ` · app Android ${ai.version} (API ${ai.sdk})` : ''}`), h('div', { style: 'margin-top:6px' }, 'Envío directo por LAN, links en la nube y tickets firmados. Sin cuentas, sin rastreo.')));
  root.append(h('div', { class: 'card' }, h('h3', {}, 'Estado del motor'), h('dl', { class: 'kv' }, h('dt', {}, 'En marcha'), h('dd', {}, fmtT(d.uptime)), h('dt', {}, 'Puerto LAN'), h('dd', { class: 'mono' }, String(d.lan_port)), h('dt', {}, 'Descargas'), h('dd', { class: 'mono' }, String(d.download_dir)), h('dt', {}, 'Config'), h('dd', { class: 'mono' }, S.cfgPath || '—'))));
  root.append(h('div', { class: 'list' }, h('a', { class: 'row', href: 'https://github.com/correo415415/uni-share', target: '_blank', rel: 'noopener' }, h('span', { class: 'ic', html: ico('link') }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, 'Código fuente'), h('div', { class: 'sub' }, 'github.com/correo415415/uni-share')), h('span', { class: 'chev', html: ico('chev') })),
    h('a', { class: 'row', href: '/', target: mobile ? '_self' : '_blank' }, h('span', { class: 'ic', html: ico('globe') }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, 'Interfaz de escritorio'), h('div', { class: 'sub' }, 'La misma app con la vista completa')), h('span', { class: 'chev', html: ico('chev') }))));
  return root;
}

// ───────────────────────── flows (bottom sheets) ─────────────────────────
function bigQr(data, caption) {
  if (!data) return;
  sheet({ title: caption || 'Código QR', cls: 'full', body: h('div', { style: 'display:flex;flex-direction:column;align-items:center;gap:14px;padding-top:8px' },
    h('div', { class: 'qrbox big' }, h('img', { src: '/api/qr?data=' + encodeURIComponent(data), alt: 'QR' })),
    h('div', { class: 'linkbox', style: 'width:100%' }, h('span', { class: 't' }, data), h('button', { class: 'ib', 'aria-label': 'Copiar', html: ico('copy'), onclick: () => copy(data) })),
    h('div', { class: 'btns', style: 'width:100%;margin:0' }, h('button', { class: 'btn primary', html: ico('share') + 'Compartir', onclick: () => shareText(data, caption) }))) });
}
function openShareSheet(j) {
  const items = []; if (j.link) items.push(['link', 'Link', j.link]); if (j.ticket_uri) items.push(['ticket', 'Ticket', j.ticket_uri]);
  if (!items.length) return toast('Esta transferencia no tiene nada que compartir', 'info');
  let cur = items[0][0];
  const body = h('div');
  const draw = () => {
    const it = items.find(x => x[0] === cur); body.innerHTML = '';
    if (items.length > 1) body.append(h('div', { class: 'seg' }, items.map(x => h('button', { class: x[0] === cur ? 'active' : '', onclick: () => { cur = x[0]; draw(); }, html: ico(x[0]) + x[1] }))));
    body.append(h('div', { class: 'qrbox', onclick: () => bigQr(it[2], j.name) }, h('img', { src: '/api/qr?data=' + encodeURIComponent(it[2]), alt: 'QR' })));
    body.append(h('div', { class: 'linkbox' }, h('span', { class: 't' }, it[2]), h('button', { class: 'ib', 'aria-label': 'Copiar', html: ico('copy'), onclick: () => copy(it[2], `${it[1]} copiado`) })));
    if (cur === 'ticket') body.append(h('p', { class: 'hint', style: 'margin:0 0 10px' }, 'El ticket incluye nombre, tamaño, hash de cada archivo y la firma de este dispositivo; quien lo abra podrá verificar que nada ha cambiado.'));
    const btns = h('div', { class: 'btns', style: 'margin:0' });
    btns.append(h('button', { class: 'btn', html: ico('copy') + 'Copiar', onclick: () => copy(it[2], `${it[1]} copiado`) }));
    if (cur === 'ticket' && j.ticket_path && mobile) btns.append(h('button', { class: 'btn primary', html: ico('share') + 'Enviar .unishare', onclick: () => Android.shareFile(j.ticket_path, `${j.name}.unishare`) }));
    else if (cur === 'ticket' && !mobile) btns.append(h('a', { class: 'btn primary', href: `/api/ticket/file?uri=${encodeURIComponent(it[2])}&name=${encodeURIComponent(j.name)}`, download: `${j.name}.unishare`, html: ico('dl') + 'Guardar .unishare' }));
    else btns.append(h('button', { class: 'btn primary', html: ico('share') + 'Compartir', onclick: () => shareText(it[2], j.name) }));
    body.append(btns);
  };
  draw();
  sheet({ title: `Compartir «${j.name}»`, body });
}

/** New transfer sheet: lan | global | download | ticket. preset: {path, device, target, url, links} */
function openNew(mode = 'lan', preset = {}) {
  const d = S.data || { devices: [] }; const c = S.cfg || { global: {} };
  const st = { mode, path: preset.path || '', device: preset.device || '', target: preset.target || '', url: preset.url || '', dest: '', links: preset.links || '' };
  const body = h('div'); const err = h('span', { class: 'err' }); const okBtn = h('button', { class: 'btn primary' });
  const ov = sheet({ title: 'Nueva transferencia', cls: 'full', body, foot: h('div', { style: 'display:flex;gap:10px;align-items:center;width:100%' }, err, okBtn) });
  const MODES = [['lan', 'send', 'LAN'], ['global', 'globe', 'Link'], ['download', 'dl', 'Descargar'], ['ticket', 'ticket', 'Ticket']];
  const field = (label, ctl, sub) => h('div', { class: 'field' }, h('label', {}, label), ctl, sub ? h('div', { class: 'sub' }, sub) : null);
  const picked = () => h('button', { class: `picked${st.path ? '' : ' none'}`, id: 'f-pick', html: ico(st.path ? 'file' : 'folder') + `<span class="t">${esc(st.path || 'Elegir archivo o carpeta…')}</span>` + (st.path ? ico('x') : '') });
  const toggle = (id, label, sub, on) => h('button', { class: 'sw', 'data-tg': id, role: 'switch', 'aria-checked': on ? 'true' : 'false', style: 'padding:8px 0' }, h('span', { class: 'body' }, h('div', { class: 'ttl' }, label), sub ? h('div', { class: 'sub' }, sub) : null), h('span', { class: `tg${on ? ' on' : ''}` }));
  const tg = { ticket: true, compress: !!c.compress_folders, force: false };
  const draw = () => {
    body.innerHTML = ''; err.textContent = '';
    body.append(h('div', { class: 'seg' }, MODES.map(([m, i, l]) => h('button', { class: st.mode === m ? 'active' : '', onclick: () => { st.mode = m; draw(); }, html: ico(i) + l }))));
    if (st.mode === 'lan') {
      body.append(field('Qué enviar', picked()));
      const devs = d.devices || [];
      body.append(h('label', { class: 'hint', style: 'display:block;margin-bottom:6px;font-size:var(--fs-sm);color:var(--fg-2);font-weight:500' }, 'A quién'));
      body.append(devs.length ? h('div', { class: 'devgrid' }, devs.map(dv => h('button', { class: `devc${st.device === dv.name ? ' active' : ''}`, 'data-d': dv.name, onclick: () => { st.device = dv.name; st.target = ''; draw(); } }, h('b', {}, dv.name), h('code', {}, h('span', { class: `dot${dv.requires_pin ? ' pin' : ''}` }), `${dv.addresses[0]}:${dv.port}`))))
        : h('div', { class: 'alert', style: 'margin-bottom:10px', html: ico('info') + '<span>No se ve ningún dispositivo. Escribe su IP o pega su ticket de emparejamiento.</span>' }));
      body.append(field('O dirección / ticket', h('div', { class: 'with-btn' }, h('input', { id: 'f-target', value: st.target, placeholder: '192.168.1.20:7777 o unishare:…', autocomplete: 'off', oninput: e => { st.target = e.target.value; if (st.target) st.device = ''; } }), mobile ? h('button', { class: 'btn', id: 'f-qr', 'aria-label': 'Escanear', html: ico('qr') }) : null)));
      body.append(h('div', { class: 'grid2' }, field('PIN (si lo pide)', h('input', { id: 'f-pin', type: 'password', inputmode: 'numeric', placeholder: '····' })), h('div')));
      body.append(toggle('compress', 'Comprimir carpetas', 'Un solo archivo en vez de muchos', tg.compress));
      okBtn.innerHTML = ico('send') + 'Enviar';
    } else if (st.mode === 'global') {
      body.append(field('Qué subir', picked()));
      body.append(h('div', { class: 'grid2' }, field('Contraseña', h('input', { id: 'f-pw', type: 'password', placeholder: 'opcional' })), field('Caduca en', h('input', { id: 'f-exp', type: 'number', min: 1, value: c.global.expiry_days || 7 }), 'días')));
      body.append(field('Mensaje', h('input', { id: 'f-msg', placeholder: 'opcional · se incluye en el ticket' })));
      body.append(toggle('ticket', 'Crear ticket .unishare', 'Firmado, con hashes para verificar', tg.ticket));
      body.append(toggle('compress', 'Comprimir carpetas', null, tg.compress));
      body.append(h('p', { class: 'hint', style: 'margin:10px 0 0' }, `Servicio: ${c.global.backend || '—'}`));
      okBtn.innerHTML = ico('ul') + 'Subir';
    } else if (st.mode === 'download') {
      const prev = h('div', { id: 'f-prev' });
      body.append(field('Link o ticket', h('div', { class: 'with-btn' }, h('textarea', { id: 'f-url', placeholder: 'https://… o unishare:…', style: 'min-height:72px', oninput: e => { st.url = e.target.value; previewTicket(st.url, prev); } }, st.url), mobile ? h('button', { class: 'btn', id: 'f-qr', 'aria-label': 'Escanear', html: ico('qr') }) : null)));
      body.append(prev);
      body.append(field('Contraseña', h('input', { id: 'f-pw', type: 'password', placeholder: 'si el link la tiene' })));
      if (!mobile) body.append(field('Guardar en', h('button', { class: `picked${st.dest ? '' : ' none'}`, id: 'f-dest', html: ico('folder') + `<span class="t">${esc(st.dest || String(d.download_dir || 'carpeta de descargas'))}</span>` })));
      body.append(toggle('force', 'Descargar aunque el análisis avise', 'Solo si confías en el origen', tg.force));
      if (st.url) previewTicket(st.url, prev);
      okBtn.innerHTML = ico('dl') + 'Descargar';
    } else {
      body.append(h('div', { class: 'alert', html: ico('ticket') + '<span>Un ticket agrupa uno o varios links ya existentes en un archivo .unishare firmado por este dispositivo.</span>' }));
      body.append(field('Links (uno por línea)', h('textarea', { id: 'f-links', placeholder: 'https://…\nhttps://…', oninput: e => { st.links = e.target.value; } }, st.links)));
      body.append(h('div', { class: 'grid2' }, field('Nombre', h('input', { id: 'f-name', placeholder: 'opcional' })), field('Contraseña', h('input', { id: 'f-tpw', type: 'password', placeholder: 'de los links' }))));
      body.append(field('Mensaje', h('input', { id: 'f-tmsg', placeholder: 'opcional' })));
      okBtn.innerHTML = ico('ticket') + 'Crear ticket';
    }
    const pk = $('#f-pick', body); if (pk) pk.onclick = async () => { if (st.path) { st.path = ''; return draw(); } const p = await pickPath({ dirsOnly: false, start: st.path }); if (p) { st.path = p; draw(); } };
    const ds = $('#f-dest', body); if (ds) ds.onclick = async () => { const p = await pickPath({ dirsOnly: true, start: st.dest || String(d.download_dir || ''), title: 'Guardar en' }); if (p) { st.dest = p; draw(); } };
    const qr = $('#f-qr', body); if (qr) qr.onclick = () => { ov.close(); Android.scanQr(); };
    $$('[data-tg]', body).forEach(b => b.onclick = () => { tg[b.dataset.tg] = !tg[b.dataset.tg]; b.classList.toggle('on'); b.querySelector('.tg').classList.toggle('on', tg[b.dataset.tg]); b.setAttribute('aria-checked', tg[b.dataset.tg]); });
  };
  draw();
  okBtn.onclick = async () => {
    err.textContent = ''; okBtn.disabled = true;
    try {
      let r;
      if (st.mode === 'lan') {
        if (!st.path) throw new Error('Elige qué enviar');
        let target = st.target.trim(), fingerprint = null, port = null;
        if (!target) { const dv = (d.devices || []).find(x => x.name === st.device); if (!dv) throw new Error('Elige un dispositivo o escribe su dirección'); target = String(dv.addresses[0]); port = dv.port; fingerprint = dv.fingerprint; }
        r = await api('/api/send-lan', { method: 'POST', body: { path: st.path, target, fingerprint, port, pin: $('#f-pin', body).value || null, compress: tg.compress } });
      } else if (st.mode === 'global') {
        if (!st.path) throw new Error('Elige qué subir');
        r = await api('/api/send-global', { method: 'POST', body: { path: st.path, password: $('#f-pw', body).value || null, expiry_days: Number($('#f-exp', body).value) || null, compress: tg.compress, ticket: tg.ticket, message: $('#f-msg', body).value || null } });
      } else if (st.mode === 'download') {
        if (!st.url.trim()) throw new Error('Pega un link o un ticket');
        r = await api('/api/download', { method: 'POST', body: { url: st.url.trim(), password: $('#f-pw', body).value || null, dest: st.dest || null, force: tg.force } });
      } else {
        const links = st.links.split(/\s+/).map(s => s.trim()).filter(Boolean);
        if (!links.length) throw new Error('Añade al menos un link');
        const t = await api('/api/ticket/create', { method: 'POST', body: { links, name: $('#f-name', body).value || null, password: $('#f-tpw', body).value || null, message: $('#f-tmsg', body).value || null } });
        ov.close(); toast('Ticket creado', 'ok');
        return openShareSheet({ name: t.ticket.name || 'ticket', ticket_uri: t.uri, ticket_path: String(t.path) });
      }
      ov.close(); vib(20);
      if (r && r.job) push({ kind: 'job', id: r.job.id, title: KIND[r.job.kind] || 'Transferencia' });
    } catch (e) { err.textContent = e.message; okBtn.disabled = false; }
  };
  return ov;
}
let prevT = null;
function previewTicket(text, el) {
  clearTimeout(prevT); el.innerHTML = ''; const t = (text || '').trim(); if (!/^unishare:/i.test(t)) return;
  prevT = setTimeout(async () => {
    try {
      const r = await api('/api/ticket/parse', { method: 'POST', body: { data: t } }); const tk = r.ticket; const sig = r.signature || {};
      const sigCls = sig.status === 'valid' ? 'ok' : sig.status === 'invalid' ? 'err' : 'warn';
      const sigTxt = sig.status === 'valid' ? `Firmado por ${sig.signer} (${sig.fingerprint})` : sig.status === 'invalid' ? `Firma NO válida: ${sig.reason}` : 'Sin firma';
      el.append(h('div', { class: `alert ${r.expired ? 'err' : sigCls}`, html: ico(r.expired ? 'x' : sig.status === 'valid' ? 'shield' : 'info') + `<span><b>${esc(tk.name)}</b> · ${fmtB(tk.total_size)} · ${tk.files.length} archivo(s)${tk.sender ? ' · de ' + esc(tk.sender) : ''}<br>${esc(sigTxt)}${r.expired ? '<br><b>Caducado</b>' : tk.expires ? '<br>caduca ' + esc(fmtDate(Date.parse(tk.expires))) : ''}${tk.message ? '<br><i>' + esc(tk.message) + '</i>' : ''}</span>` }));
    } catch (e) { el.append(h('div', { class: 'alert err', html: ico('x') + `<span>${esc(e.message)}</span>` })); }
  }, 250);
}

let pickWaiter = null;
/** Path picker. On Android (files) → system document picker via the bridge, result comes back through androidEvent('picked').
    Otherwise → server-side browser (the engine reads local files). Resolves with a path or null. */
function pickPath({ dirsOnly = false, start = '', title } = {}) {
  if (mobile && !dirsOnly) return new Promise(res => { pickWaiter = res; try { Android.pickFiles(); } catch { pickWaiter = null; res(null); } });
  return new Promise(res => {
    let out = null; let cur = start || '';
    const list = h('div', { class: 'list' }); const crumb = h('div', { class: 'linkbox', style: 'margin-bottom:10px' }); const roots = h('div', { class: 'chips' });
    const body = h('div', {}, roots, crumb, list);
    const ov = sheet({ title: title || (dirsOnly ? 'Elegir carpeta' : 'Elegir archivo o carpeta'), cls: 'full', body, onClose: () => res(out),
      foot: h('div', { class: 'btns', style: 'margin:0;width:100%' }, h('button', { class: 'btn', onclick: () => ov.close() }, 'Cancelar'), h('button', { class: 'btn primary', id: 'pk-ok', html: ico('check') + (dirsOnly ? 'Usar esta carpeta' : 'Enviar esta carpeta'), onclick: () => { out = cur; ov.close(); } })) });
    const load = async p => {
      list.innerHTML = '<div class="empty" style="padding:24px"><p>Cargando…</p></div>';
      try {
        const r = await api(`/api/fs?path=${encodeURIComponent(p || '')}&dirs_only=${dirsOnly}`); cur = String(r.path);
        crumb.innerHTML = ''; crumb.append(h('span', { class: 't' }, cur));
        roots.innerHTML = ''; for (const rt of r.roots || []) { const n = String(rt).split(/[\\/]/).filter(Boolean).pop() || String(rt); roots.append(h('button', { class: `chip${String(rt) === cur ? ' active' : ''}`, onclick: () => load(String(rt)) }, n)); }
        list.innerHTML = '';
        if (r.parent) list.append(h('button', { class: 'row', onclick: () => load(String(r.parent)) }, h('span', { class: 'ic', html: ico('up') }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, '..'), h('div', { class: 'sub' }, 'Carpeta superior'))));
        if (!r.entries.length) list.append(h('div', { class: 'row hint' }, dirsOnly ? 'Sin subcarpetas' : 'Carpeta vacía'));
        for (const e of r.entries) list.append(h('button', { class: 'row', onclick: () => { if (e.dir) load(String(e.path)); else { out = String(e.path); ov.close(); } } },
          h('span', { class: 'ic', html: ico(e.dir ? 'folder' : 'file') }), h('span', { class: 'body' }, h('div', { class: 'ttl' }, e.name), h('div', { class: 'sub' }, e.dir ? 'Carpeta' : fmtB(e.size))), e.dir ? h('span', { class: 'chev', html: ico('chev') }) : null));
      } catch (e) { list.innerHTML = ''; list.append(h('div', { class: 'alert err', html: ico('x') + `<span>${esc(e.message)}</span>` })); }
    };
    load(cur);
  });
}

// ───────────────────────── job actions ─────────────────────────
function jobMenu(j) {
  const act = isActive(j);
  menuSheet(j.name, [
    (j.link || j.ticket_uri) && { icon: 'share', label: 'Compartir link / ticket', fn: () => openShareSheet(j) },
    j.link && { icon: 'copy', label: 'Copiar link', sub: j.link, fn: () => copy(j.link, 'Link copiado') },
    act && { icon: 'stop', label: 'Cancelar', danger: true, fn: () => cancelJob(j) },
    !act && j.retryable && { icon: 'refresh', label: 'Reintentar', fn: () => retryJob(j) },
    !act && (j.kind === 'lan_receive' || j.kind === 'download') && j.state === 'completed' && { icon: 'shield', label: 'Analizar de nuevo', fn: () => rescanJob(j) },
    !act && (j.kind === 'lan_receive' || j.kind === 'download') && j.state === 'completed' && mobile && j.saved && j.saved.length && { icon: 'folder', label: 'Copiar a la carpeta de descargas', fn: () => exportToSaf(j) },
    !act && j.link && { icon: 'dl', label: 'Descargar de nuevo', fn: () => openNew('download', { url: j.link }) },
    !act && { icon: 'trash', label: 'Quitar de la lista', danger: true, fn: () => removeJob(j) }]);
}
async function cancelJob(j) { if (await confirmSheet(`Se cancela «${j.name}».`, 'Cancelar transferencia', true)) api(`/api/jobs/${j.id}/cancel`, { method: 'POST' }).then(() => toast('Cancelada', 'info')).catch(e => toast(e.message, 'err')); }
async function removeJob(j) { try { await api(`/api/jobs/${j.id}`, { method: 'DELETE' }); if (S.stack.length && S.stack[S.stack.length - 1].id === j.id) pop(); } catch (e) { toast(e.message, 'err'); } }
async function retryJob(j) { try { const r = await api(`/api/jobs/${j.id}/retry`, { method: 'POST' }); toast('Reintentando…', 'ok'); S.stack = []; push({ kind: 'job', id: r.id, title: KIND[j.kind] }); } catch (e) { toast(e.message, 'err'); } }
async function rescanJob(j) {
  toast('Analizando…', 'info', 2000);
  try { const r = await api(`/api/jobs/${j.id}/scan`, { method: 'POST' }); const sv = SCAN[r.severity] || SCAN.info;
    sheet({ title: 'Resultado del análisis', body: h('div', {}, h('div', { class: `alert ${sv[0] === 'danger' ? 'err' : sv[0]}`, html: ico('shield') + `<span><b>${sv[1]}</b> · ${esc(r.summary)}</span>` }), r.detail ? h('pre', { class: 'log' }, r.detail) : null) });
  } catch (e) { toast(e.message, 'err'); }
}

// ───────────────────────── wiring, theme, init ─────────────────────────
function wire(root, top) {
  wireCommon(root);
  if (top) { if (top.kind === 'job') wireJob(root, top.id); else if (top.kind === 'security') wireSecurity(root); else if (top.kind === 'history') wireHistory(root); return; }
  ({ home: wireHome, jobs: wireJobs, devices: wireDevices, share: wireShare, settings: wireSettings })[S.tab](root);
}
function setTheme(t) { document.documentElement.dataset.theme = t; localStorage.mtheme = t; $('#t-theme').innerHTML = ico(t === 'light' ? 'sun' : 'moon'); const m = $('meta[name=theme-color]'); if (m) m.content = t === 'light' ? '#f4f5f7' : '#151719'; }
function toggleTheme() { setTheme(document.documentElement.dataset.theme === 'light' ? 'dark' : 'light'); render(); }
function init() {
  // Deep link for reviews/screenshots: /m#jobs or /m#home&theme=light (also honoured when the page is reloaded).
  const hash = location.hash.replace(/^#/, ''); const hp = new URLSearchParams(hash.replace(/^([a-z]+)(&|$)/, 'tab=$1$2'));
  if (hp.get('tab') && SCREENS[hp.get('tab')]) { S.tab = hp.get('tab'); localStorage.mtab = S.tab; }
  S.shot = !!hp.get('shot'); if (S.shot) document.documentElement.classList.add('shot'); // headless captures: no animations, dark unless theme=light
  setTheme(hp.get('theme') || (S.shot ? 'dark' : localStorage.mtheme || (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark')));
  $$('#tabs button').forEach(b => b.onclick = () => { if (S.tab === b.dataset.tab && !S.stack.length) { try { $('#page').scrollTo({ top: 0, behavior: 'smooth' }); } catch { $('#page').scrollTop = 0; } } else go(b.dataset.tab); });
  $('#fab').onclick = () => openNew(S.tab === 'share' ? 'global' : 'lan');
  $('#t-back').onclick = pop;
  $('#t-theme').onclick = toggleTheme;
  const q = $('#t-qr'); q.hidden = !mobile; q.onclick = () => Android.scanQr();
  if (mobile) document.documentElement.classList.add('android');
  history.replaceState({ depth: 0 }, '');
  document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible' && !es) poll(); });
  render(); loadCfg();
  if (S.shot) poll(); else connect(); // shot=1: one /api/state fetch, no SSE (headless screenshots)
}
init();
