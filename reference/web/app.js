// ThornCue show page: playback engine, cue list, inspector, trim editor, sharing.
'use strict';

const $ = id => document.getElementById(id);
const SHOW = JSON.parse($('show-data').textContent);
const CODE = SHOW.code;
const PERMS = JSON.parse(document.body.dataset.perms || '{}');
const API = `/show/${encodeURIComponent(CODE)}`;

const CUE_DEFAULTS = {
  type: 'audio', filename: '', volume: 1, fade_in: 0, fade_out: 0, pre_wait: 0,
  loop: false, auto_continue: false, duration: 0, trim_start: 0, trim_end: null, cuts: [],
};
const normaliseCue = c => ({
  ...CUE_DEFAULTS, ...c,
  cuts: Array.isArray(c.cuts) ? c.cuts.map(p => [Number(p[0]), Number(p[1])]) : [],
});

let cues = SHOW.cues.map(normaliseCue);
let standby = 0;            // index GO fires next; cues.length means "end of list"
let inspectedId = cues[0]?.id ?? null;

const isAudio = c => c.type !== 'silence' && c.type !== 'fadeout';
const cueIndex = id => cues.findIndex(c => c.id === id);
const cueById = id => cues.find(c => c.id === id);
const qLabel = c => `Q${cueIndex(c.id) + 1}`;
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const r4 = x => Math.round(x * 1e4) / 1e4;

function fmtTime(s, centis = false) {
  if (!Number.isFinite(s) || s < 0) s = 0;
  if (centis) {
    const t = Math.round(s * 100);
    return `${Math.floor(t / 6000)}:${String(Math.floor(t / 100) % 60).padStart(2, '0')}.${String(t % 100).padStart(2, '0')}`;
  }
  const t = Math.floor(s);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, '0')}`;
}

function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  Object.assign(node, props);
  for (const ch of children) node.append(ch);
  return node;
}

// ---------------------------------------------------------------------------
// Toast and server requests
// ---------------------------------------------------------------------------

let toastTimer = null;
function toast(msg, isError = false) {
  const t = $('toast');
  t.textContent = msg;
  t.className = 'toast' + (isError ? ' toast--error' : '');
  t.setAttribute('role', isError ? 'alert' : 'status');
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.hidden = true; }, isError ? 6000 : 2500);
}

// Requests that change the show run one at a time, so a save can never
// overtake an add or duplicate and drop the new cue.
let queue = Promise.resolve();
function serial(fn) {
  const run = queue.then(fn, fn);
  queue = run.catch(() => { });
  return run;
}

async function request(url, opts = {}) {
  let res;
  try { res = await fetch(url, opts); }
  catch { throw new Error("couldn't reach the server"); }
  if (res.redirected && res.url.includes('/login')) throw new Error('your session has ended. Reload the page and sign in');
  let data = null;
  try { data = await res.json(); } catch { /* non-JSON error page */ }
  if (!res.ok || !data || data.ok === false) {
    if (res.status === 403) throw new Error("you don't have permission to do that");
    throw new Error(data?.error || `the server returned ${res.status}`);
  }
  return data;
}

const postJSON = (path, body = {}) => serial(() => request(API + path, {
  method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
}));
const postForm = (path, form) => serial(() => request(API + path, { method: 'POST', body: form }));

function uploadWithProgress(path, form, onProgress) {
  return serial(() => new Promise((resolve, reject) => {
    const x = new XMLHttpRequest();
    x.open('POST', API + path);
    x.responseType = 'json';
    x.upload.onprogress = e => { if (e.lengthComputable) onProgress(e.loaded / e.total); };
    x.onload = () => {
      const d = x.response;
      if (x.status === 413) reject(new Error('the file is larger than the server allows'));
      else if (x.status >= 400 || !d || !d.ok) reject(new Error(d?.error || `the server returned ${x.status}`));
      else resolve(d);
    };
    x.onerror = () => reject(new Error("couldn't reach the server"));
    x.send(form);
  }));
}

async function copyText(text) {
  try { await navigator.clipboard.writeText(text); return true; }
  catch {
    // Clipboard API needs HTTPS; fall back for plain-HTTP LAN installs.
    const ta = el('textarea', { value: text });
    document.body.append(ta);
    ta.select();
    const ok = document.execCommand('copy');
    ta.remove();
    return ok;
  }
}

function downloadBlob(blob, name) {
  const a = el('a', { href: URL.createObjectURL(blob), download: name });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 10000);
}

const safeFileName = s => s.replace(/[\\/:*?"<>|]+/g, '').replace(/\s+/g, ' ').trim();

// ---------------------------------------------------------------------------
// Edits: trim and cuts are stored as source-file seconds; the file is never changed
// ---------------------------------------------------------------------------

const MIN_LEN = 0.05;
const outPoint = (c, dur) => (c.trim_end == null ? dur : Math.min(c.trim_end, dur));

function subtractCuts(ranges, cuts) {
  let out = ranges;
  for (const [a, b] of cuts) {
    const next = [];
    for (const [s, e] of out) {
      if (b <= s || a >= e) { next.push([s, e]); continue; }
      if (a > s) next.push([s, a]);
      if (b < e) next.push([b, e]);
    }
    out = next;
  }
  return out.filter(([s, e]) => e - s > 0.001);
}

function keptSegments(c, dur) {
  const start = clamp(c.trim_start, 0, dur);
  let end = outPoint(c, dur);
  if (end <= start) end = dur;
  return subtractCuts([[start, end]], c.cuts);
}

const segmentsLength = segs => segs.reduce((n, [s, e]) => n + e - s, 0);

// Position in the edited audio -> position in the source file.
function toSourceTime(segs, t) {
  for (const [s, e] of segs) {
    if (t <= e - s) return s + t;
    t -= e - s;
  }
  return segs.length ? segs[segs.length - 1][1] : 0;
}

function mergeCuts(cuts) {
  const sorted = cuts.map(p => [r4(p[0]), r4(p[1])]).sort((a, b) => a[0] - b[0]);
  const out = [];
  for (const p of sorted) {
    const last = out[out.length - 1];
    if (last && p[0] <= last[1]) last[1] = Math.max(last[1], p[1]);
    else out.push(p);
  }
  return out;
}

// Copy the kept segments into one buffer, with a 4ms ramp at each join so cuts don't click.
function renderSegments(src, segs) {
  const sr = src.sampleRate;
  const lens = segs.map(([s, e]) => Math.max(0, Math.round(e * sr) - Math.round(s * sr)));
  const total = Math.max(1, lens.reduce((a, b) => a + b, 0));
  const out = new AudioBuffer({ length: total, numberOfChannels: src.numberOfChannels, sampleRate: sr });
  const ramp = Math.round(sr * 0.004);
  for (let ch = 0; ch < src.numberOfChannels; ch++) {
    const from = src.getChannelData(ch);
    const to = out.getChannelData(ch);
    let pos = 0;
    segs.forEach(([s], i) => {
      const start = Math.round(s * sr);
      const len = Math.min(lens[i], from.length - start);
      to.set(from.subarray(start, start + len), pos);
      const r = Math.min(ramp, len >> 1);
      if (i > 0) for (let k = 0; k < r; k++) to[pos + k] *= k / r;
      if (i < segs.length - 1) for (let k = 0; k < r; k++) to[pos + len - 1 - k] *= k / r;
      pos += len;
    });
  }
  return out;
}

// ---------------------------------------------------------------------------
// Audio engine
// ---------------------------------------------------------------------------

let ctx = null;
let master = null;
let masterVolume = 1;
let paused = false;

function audio() {
  if (!ctx) {
    ctx = new (window.AudioContext || window.webkitAudioContext)();
    master = ctx.createGain();
    master.gain.value = masterVolume;
    master.connect(ctx.destination);
  }
  return ctx;
}

// Call from user actions: browsers only start audio after a gesture.
function wake() {
  const ac = audio();
  if (ac.state === 'suspended') ac.resume();
  paused = false;
  return ac;
}

const sources = new Map();   // filename -> Promise<AudioBuffer>
const decoded = new Map();   // filename -> AudioBuffer, once loaded
const loadErrors = new Map(); // filename -> message
const renders = new Map();   // cue id -> { key, segs, buffer }

function loadSource(c) {
  const fn = c.filename;
  if (!fn) return Promise.reject(new Error('this cue has no audio file'));
  if (!sources.has(fn)) {
    const p = fetch(`${API}/audio/${encodeURIComponent(fn)}`, { cache: 'no-cache' })
      .then(r => {
        if (!r.ok) throw new Error(r.status === 404 ? 'its audio file is missing on the server' : `the server returned ${r.status}`);
        return r.arrayBuffer();
      })
      .then(ab => audio().decodeAudioData(ab))
      .then(buf => { decoded.set(fn, buf); loadErrors.delete(fn); return buf; },
        err => {
          sources.delete(fn);
          loadErrors.set(fn, err.message && !/decode/i.test(err.message) ? err.message : "the browser couldn't decode it");
          throw new Error(loadErrors.get(fn));
        });
    sources.set(fn, p);
  }
  return sources.get(fn);
}

function forgetSource(fn) {
  sources.delete(fn);
  decoded.delete(fn);
  loadErrors.delete(fn);
}

const editKey = c => `${c.filename}|${c.trim_start}|${c.trim_end}|${JSON.stringify(c.cuts)}`;

function playable(c, src) {
  const key = editKey(c);
  const hit = renders.get(c.id);
  if (hit && hit.key === key) return hit;
  const segs = keptSegments(c, src.duration);
  const whole = segs.length === 1 && segs[0][0] === 0 && segs[0][1] >= src.duration - 1e-6;
  const r = { key, segs, buffer: whole ? src : renderSegments(src, segs) };
  renders.set(c.id, r);
  return r;
}

function playedLength(c) {
  if (!isAudio(c)) return c.duration;
  const src = decoded.get(c.filename);
  return src ? segmentsLength(keptSegments(c, src.duration)) : null;
}

const running = new Map();  // cue id -> { cue, node, gain, startAt, length, loop, buffer, control, stopped, fading }
let lastStarted = null;

async function playCue(c) {
  const ac = wake();

  if (c.type === 'silence') {
    stopAll({ keepAudition: true });
    startClock(c, c.duration);
    return;
  }
  if (c.type === 'fadeout') {
    for (const [id, e] of running) if (!e.control) stopCue(id, c.duration);
    startClock(c, c.duration);
    return;
  }

  let src;
  try { src = await loadSource(c); }
  catch (e) {
    toast(`${qLabel(c)} "${c.name}" can't play: ${e.message}.`, true);
    renderRow(c.id);
    return;
  }
  const r = playable(c, src);
  stopCue(c.id, 0);

  const node = ac.createBufferSource();
  node.buffer = r.buffer;
  node.loop = c.loop;
  const gain = ac.createGain();
  const now = ac.currentTime;
  const startAt = now + c.pre_wait;
  const len = r.buffer.duration;
  scheduleGain(gain.gain, c, now, startAt, len);
  node.connect(gain).connect(master);
  node.start(startAt);

  const entry = { cue: c, node, gain, startAt, length: len, loop: c.loop, buffer: r.buffer };
  node.onended = () => finish(c.id, entry);
  running.set(c.id, entry);
  lastStarted = c.id;
  renderRow(c.id);
  refreshTransport();
  startTicking();
}

// Fades run on the audio clock, so they stay in time when the show is paused.
function scheduleGain(g, c, now, startAt, len) {
  const vol = c.volume;
  g.cancelScheduledValues(now);
  if (c.fade_in > 0 && now < startAt + c.fade_in) {
    // Mid fade-in (a live volume change): carry on from the current level.
    g.setValueAtTime(now > startAt ? g.value : 0, Math.max(now, startAt));
    g.linearRampToValueAtTime(vol, startAt + c.fade_in);
  } else {
    g.setValueAtTime(vol, now);
  }
  if (!c.loop && c.fade_out > 0) {
    const fadeStart = Math.max(startAt + Math.min(c.fade_in, len), startAt + len - c.fade_out, now);
    if (fadeStart < startAt + len) {
      g.setValueAtTime(vol, fadeStart);
      g.linearRampToValueAtTime(0, startAt + len);
    }
  }
}

// Silence and fade-out cues: a silent source whose end is timed by the audio clock.
function startClock(c, seconds) {
  const ac = audio();
  stopCue(c.id, 0);
  const node = ac.createBufferSource();
  node.buffer = ac.createBuffer(1, 1, ac.sampleRate);
  node.loop = true;
  const gain = ac.createGain();
  gain.gain.value = 0;
  node.connect(gain).connect(ac.destination);
  const startAt = ac.currentTime;
  node.start(startAt);
  node.stop(startAt + Math.max(0.01, seconds));
  const entry = { cue: c, node, gain, startAt, length: seconds, loop: false, control: true };
  node.onended = () => finish(c.id, entry);
  running.set(c.id, entry);
  renderRow(c.id);
  refreshTransport();
  startTicking();
}

function finish(id, entry) {
  if (running.get(id) !== entry) return;
  running.delete(id);
  renderRow(id);
  refreshTransport();
  if (entry.cue.auto_continue && !entry.stopped) {
    const i = cueIndex(id);
    if (i >= 0 && i + 1 < cues.length) fireIndex(i + 1);
  }
}

function stopCue(id, fade = 0) {
  const e = running.get(id);
  if (!e) return;
  e.stopped = true;
  if (fade > 0 && ctx && !e.control) {
    const now = ctx.currentTime;
    const g = e.gain.gain;
    g.cancelScheduledValues(now);
    g.setValueAtTime(g.value, now);
    g.linearRampToValueAtTime(0, now + fade);
    try { e.node.stop(now + fade + 0.02); } catch { /* already stopped */ }
    e.fading = true;
    renderRow(id);
    refreshTransport();
    return;
  }
  try { e.node.stop(); } catch { /* already stopped */ }
  running.delete(id);
  renderRow(id);
  refreshTransport();
}

function stopAll({ keepAudition = false } = {}) {
  for (const id of [...running.keys()]) stopCue(id, 0);
  if (!keepAudition) stopAudition();
  if (ctx && paused) { ctx.resume(); paused = false; }
  refreshTransport();
}

function togglePause() {
  if (!ctx) return;
  if (paused) { ctx.resume(); paused = false; }
  else { ctx.suspend(); paused = true; }
  refreshTransport();
}

function hardStop() {
  stopAll();
  if (ctx) { ctx.close(); ctx = null; master = null; }
  paused = false;
  refreshTransport();
  toast('Audio engine restarted.');
}

// Change a running cue's level while it plays, keeping its scheduled fade-out.
function applyLiveVolume(c) {
  const e = running.get(c.id);
  if (!e || e.control || e.fading || !ctx) return;
  const now = ctx.currentTime;
  if (!c.loop && c.fade_out > 0 && now > e.startAt + e.length - c.fade_out) return; // already fading out
  scheduleGain(e.gain.gain, c, now, e.startAt, e.length);
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

function go() {
  if (!cues.length) { toast('There are no cues to play.', true); return; }
  if (standby >= cues.length) { toast('End of the cue list. Use Back to top or click a cue.'); return; }
  fireIndex(standby);
}

function fireIndex(i) {
  standby = i + 1;
  playCue(cues[i]);
  renderStandby();
}

function setStandby(i, { inspectIt = true } = {}) {
  standby = clamp(i, 0, cues.length);
  if (inspectIt && cues[standby]) inspect(cues[standby].id);
  renderStandby();
}

function moveStandby(dir) {
  if (!cues.length) return;
  setStandby(clamp(standby + dir, 0, cues.length - 1));
}

// Keep standby on the same cue across inserts, deletes and reorders.
function keepStandby(mutate) {
  const id = cues[standby]?.id;
  mutate();
  const i = id ? cueIndex(id) : -1;
  standby = i >= 0 ? i : Math.min(standby, cues.length);
}

function jumpTo(n) {
  if (!Number.isInteger(n) || n < 1 || n > cues.length) {
    toast(`There is no Q${n}. This show has ${cues.length} cue${cues.length === 1 ? '' : 's'}.`, true);
    return false;
  }
  inspect(cues[n - 1].id);
  fireIndex(n - 1);
  return true;
}

function renderStandby() {
  for (const tr of $('cue-list').rows) {
    const on = tr.dataset.id === cues[standby]?.id;
    tr.classList.toggle('is-standby', on);
    if (on) tr.scrollIntoView({ block: 'nearest' });
  }
  const c = cues[standby];
  $('standby-name').textContent = c ? `${qLabel(c)} ${c.name}` : (cues.length ? 'End of cue list' : 'No cues');
}

function refreshTransport() {
  const st = $('transport-status');
  const n = running.size;
  st.className = 'transport__status';
  if (paused) { st.textContent = 'Paused'; st.classList.add('is-paused'); }
  else if (n === 0) st.textContent = 'Ready';
  else { st.textContent = n === 1 ? 'Running' : `Running ${n} cues`; st.classList.add('is-running'); }
  $('pause-btn').textContent = paused ? 'Resume' : 'Pause';
}

// ---------------------------------------------------------------------------
// Drawing: now-playing strip and the trim waveform share one animation loop
// ---------------------------------------------------------------------------

// Canvas colours come from the theme tokens; re-read whenever data-theme changes.
const COLOR = {};
function readColors() {
  const css = getComputedStyle(document.documentElement);
  const v = name => css.getPropertyValue(name).trim();
  Object.assign(COLOR, {
    bg: v('--bg'),
    wave: v('--wave'),
    waveOff: v('--wave-off'),
    marker: v('--select-line'),
    selection: v('--select-wash'),
    playhead: v('--text'),
    progress: v('--go-text'),
  });
}
readColors();

const peakCache = new WeakMap();  // AudioBuffer -> { width, min, max }
function peaks(buf, width) {
  const hit = peakCache.get(buf);
  if (hit && hit.width === width) return hit;
  const data = buf.getChannelData(0);
  const min = new Float32Array(width);
  const max = new Float32Array(width);
  const step = data.length / width;
  for (let x = 0; x < width; x++) {
    let lo = 0, hi = 0;
    const end = Math.min(data.length, Math.floor((x + 1) * step));
    // Sample at most ~2000 points per column; plenty for display.
    const stride = Math.max(1, Math.floor((end - x * step) / 2000));
    for (let i = Math.floor(x * step); i < end; i += stride) {
      const v = data[i];
      if (v < lo) lo = v;
      if (v > hi) hi = v;
    }
    min[x] = lo;
    max[x] = hi;
  }
  const p = { width, min, max };
  peakCache.set(buf, p);
  return p;
}

function sizeCanvas(canvas) {
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(1, Math.round(canvas.clientWidth * dpr));
  const h = Math.max(1, Math.round(canvas.clientHeight * dpr));
  if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
  return { w, h, dpr };
}

let rafId = 0;
function startTicking() { if (!rafId) rafId = requestAnimationFrame(tick); }
function tick() {
  rafId = 0;
  drawNowPlaying();
  drawTrim();
  updateRunningStates();
  if (running.size || audition) rafId = requestAnimationFrame(tick);
}

function nowPlayingEntry() {
  const last = running.get(lastStarted);
  if (last && !last.control) return last;
  let found = null;
  for (const e of running.values()) if (!e.control) found = e;
  return found;
}

function drawNowPlaying() {
  const canvas = $('np-wave');
  const g = canvas.getContext('2d');
  const { w, h } = sizeCanvas(canvas);
  g.clearRect(0, 0, w, h);
  const e = nowPlayingEntry();
  if (!e || !ctx) {
    $('np-name').textContent = running.size ? 'Control cue running' : 'Nothing running';
    $('np-times').textContent = '';
    return;
  }
  const raw = ctx.currentTime - e.startAt;
  const elapsed = raw < 0 ? 0 : e.loop ? raw % e.length : Math.min(raw, e.length);
  $('np-name').textContent = `${qLabel(e.cue)} ${e.cue.name}${e.fading ? ' (fading out)' : ''}`;
  const times = $('np-times');
  if (raw < 0) times.textContent = `Starts in ${(-raw).toFixed(1)} s`;
  else times.replaceChildren(el('b', { textContent: fmtTime(elapsed) }),
    ` of ${fmtTime(e.length)}${e.loop ? ', looping' : `, ${fmtTime(e.length - elapsed)} left`}`);

  const p = peaks(e.buffer, w);
  const mid = h / 2;
  const px = Math.floor((elapsed / e.length) * w);
  for (let x = 0; x < w; x++) {
    g.fillStyle = x < px ? COLOR.progress : COLOR.wave;
    const top = mid - Math.max(0.5, -p.min[x] * mid);
    const bot = mid + Math.max(0.5, p.max[x] * mid);
    g.fillRect(x, top, 1, bot - top);
  }
}

function updateRunningStates() {
  for (const id of running.keys()) {
    const cell = $('cue-list').querySelector(`tr[data-id="${id}"] .c-state`);
    const c = cueById(id);
    if (cell && c) {
      const s = rowState(c);
      if (cell.textContent !== s.text) cell.textContent = s.text;
    }
  }
}

// ---------------------------------------------------------------------------
// Cue list
// ---------------------------------------------------------------------------

function rowNotes(c) {
  const n = [];
  if (c.type === 'silence') n.push('Stops everything, then waits');
  if (c.type === 'fadeout') n.push('Fades out everything running');
  if (isAudio(c)) {
    if (c.loop) n.push('Loop');
    if (c.fade_in) n.push(`Fade in ${c.fade_in} s`);
    if (c.fade_out) n.push(`Fade out ${c.fade_out} s`);
    if (c.pre_wait) n.push(`Wait ${c.pre_wait} s`);
    if (c.trim_start > 0 || c.trim_end != null) n.push('Trimmed');
    if (c.cuts.length) n.push(c.cuts.length === 1 ? '1 cut' : `${c.cuts.length} cuts`);
  }
  if (c.auto_continue) n.push('Auto-continue');
  return n.join(', ');
}

function rowState(c) {
  const e = running.get(c.id);
  if (e) {
    if (e.fading) return { text: 'Fading' };
    if (ctx && ctx.currentTime < e.startAt) return { text: 'Waiting' };
    return { text: 'Running' };
  }
  if (isAudio(c) && loadErrors.has(c.filename)) return { text: "Can't load", missing: true };
  return { text: '' };
}

function buildRow(c, i) {
  const tr = el('tr', { className: 'cue-row' });
  tr.dataset.id = c.id;
  tr.draggable = !!PERMS.edit;
  const len = playedLength(c);
  const state = rowState(c);
  const stateCell = el('td', { className: 'c-state', textContent: state.text });
  if (state.missing) {
    stateCell.classList.add('is-missing');
    stateCell.title = loadErrors.get(c.filename);
  }
  tr.append(
    el('td', { className: 'c-grip', textContent: PERMS.edit ? '⠿' : '', ariaHidden: 'true', title: PERMS.edit ? 'Drag to reorder' : '' }),
    el('td', { className: 'c-q num', textContent: String(i + 1) }),
    el('td', { className: 'c-name', textContent: c.name, title: c.name }),
    el('td', { className: 'c-dur num', textContent: len == null ? '' : fmtTime(len) }),
    el('td', { className: 'c-vol num', textContent: isAudio(c) ? `${Math.round(c.volume * 100)}%` : '' }),
    el('td', { className: 'c-notes', textContent: rowNotes(c), title: rowNotes(c) }),
    stateCell,
  );
  tr.classList.toggle('is-standby', i === standby);
  tr.classList.toggle('is-running', running.has(c.id));
  tr.classList.toggle('is-inspected', c.id === inspectedId);
  return tr;
}

function renderList() {
  const body = $('cue-list');
  body.replaceChildren(...cues.map(buildRow));
  $('cue-list-empty').hidden = cues.length > 0;
  $('cue-count').textContent = `${cues.length} cue${cues.length === 1 ? '' : 's'}`;
  renderStandby();
}

function renderRow(id) {
  const i = cueIndex(id);
  const old = $('cue-list').querySelector(`tr[data-id="${id}"]`);
  if (i < 0 || !old) return;
  old.replaceWith(buildRow(cues[i], i));
}

$('cue-list').addEventListener('click', e => {
  const tr = e.target.closest('tr[data-id]');
  if (tr) setStandby(cueIndex(tr.dataset.id));
});

let dragId = null;
$('cue-list').addEventListener('dragstart', e => {
  const tr = e.target.closest('tr[data-id]');
  if (!tr) return;
  dragId = tr.dataset.id;
  tr.classList.add('is-dragging');
  e.dataTransfer.effectAllowed = 'move';
  e.dataTransfer.setData('text/plain', dragId);
});
$('cue-list').addEventListener('dragend', () => {
  dragId = null;
  for (const tr of $('cue-list').querySelectorAll('.is-dragging, .is-drop-target')) tr.classList.remove('is-dragging', 'is-drop-target');
});
$('cue-list').addEventListener('dragover', e => {
  const tr = e.target.closest('tr[data-id]');
  if (!dragId || !tr) return;
  e.preventDefault();
  for (const t of $('cue-list').querySelectorAll('.is-drop-target')) if (t !== tr) t.classList.remove('is-drop-target');
  if (tr.dataset.id !== dragId) tr.classList.add('is-drop-target');
});
$('cue-list').addEventListener('drop', e => {
  const tr = e.target.closest('tr[data-id]');
  if (!dragId || !tr || tr.dataset.id === dragId) return;
  e.preventDefault();
  const from = cueIndex(dragId);
  const to = cueIndex(tr.dataset.id);
  keepStandby(() => {
    const [moved] = cues.splice(from, 1);
    cues.splice(to, 0, moved);
  });
  renderList();
  renderInspector();
  scheduleSave();
});

// ---------------------------------------------------------------------------
// Saving: every change autosaves (debounced). Order, fields and edits go in one request.
// ---------------------------------------------------------------------------

let saveTimer = null;
let saveState = 'saved';

function setSaveState(state, message) {
  saveState = state;
  const box = $('save-state');
  box.className = 'save-state' + (state === 'error' ? ' status-error' : '');
  if (state === 'error') {
    const retry = el('button', { type: 'button', className: 'btn btn--small', textContent: 'Retry' });
    retry.addEventListener('click', saveNow);
    box.replaceChildren('Not saved ', retry);
    toast(`Couldn't save: ${message}. Your changes are still here.`, true);
  } else {
    box.textContent = { pending: 'Unsaved', saving: 'Saving…', saved: 'Saved' }[state];
  }
}

function scheduleSave() {
  if (!PERMS.edit) return;
  setSaveState('pending');
  clearTimeout(saveTimer);
  saveTimer = setTimeout(saveNow, 500);
}

async function saveNow() {
  clearTimeout(saveTimer);
  saveTimer = null;
  if (!PERMS.edit) return;
  setSaveState('saving');
  const payload = {
    cues: cues.map(c => ({
      id: c.id, name: c.name, volume: c.volume, loop: c.loop, fade_in: c.fade_in, fade_out: c.fade_out,
      pre_wait: c.pre_wait, auto_continue: c.auto_continue, duration: c.duration,
      trim_start: c.trim_start, trim_end: c.trim_end, cuts: c.cuts,
    })),
  };
  if (PERMS.is_owner) payload.name = SHOW.name;
  try {
    await postJSON('/save', payload);
    if (!saveTimer) setSaveState('saved');
  } catch (e) {
    setSaveState('error', e.message);
  }
}

async function flushSave() {
  if (saveTimer || saveState === 'error') await saveNow();
}

window.addEventListener('beforeunload', e => {
  if (saveState !== 'saved') { e.preventDefault(); e.returnValue = ''; }
});

// ---------------------------------------------------------------------------
// Inspector
// ---------------------------------------------------------------------------

const F = {
  name: $('f-name'), volRange: $('f-vol-range'), vol: $('f-vol'), fadeIn: $('f-fadein'), fadeOut: $('f-fadeout'),
  preWait: $('f-prewait'), duration: $('f-duration'), loop: $('f-loop'), auto: $('f-auto'),
};

function inspect(id) {
  if (id !== inspectedId) {
    stopAudition();
    ed.marker = null;
    ed.sel = null;
  }
  inspectedId = id;
  for (const tr of $('cue-list').rows) tr.classList.toggle('is-inspected', tr.dataset.id === id);
  renderInspector();
}

function setValue(input, value) {
  if (document.activeElement === input) return; // don't fight someone typing
  if (input.type === 'checkbox') input.checked = !!value;
  else input.value = value;
}

function renderInspector() {
  const c = cueById(inspectedId);
  const body = $('insp-body');
  const actions = ['insp-audition', 'insp-duplicate', 'insp-export', 'insp-original', 'insp-replace', 'insp-delete'];
  if (!c) {
    $('insp-title').textContent = 'Inspector';
    $('insp-empty').hidden = false;
    $('insp-empty').textContent = cues.length ? 'Select a cue to see its settings.' : 'Add a cue to see its settings here.';
    body.hidden = true;
    for (const a of actions) if ($(a)) $(a).hidden = true;
    return;
  }
  const audioCue = isAudio(c);
  $('insp-title').textContent = `${qLabel(c)} ${c.name}`;
  $('insp-empty').hidden = true;
  body.hidden = false;
  for (const node of body.querySelectorAll('[data-kind]')) node.hidden = (node.dataset.kind === 'audio') !== audioCue;

  $('insp-audition').hidden = !audioCue;
  if ($('insp-duplicate')) $('insp-duplicate').hidden = false;
  $('insp-export').hidden = !audioCue;
  $('insp-original').hidden = !audioCue;
  if (audioCue) $('insp-original').href = `${API}/cue/${encodeURIComponent(c.id)}/original`;
  if ($('insp-replace')) $('insp-replace').hidden = !audioCue;
  if ($('insp-delete')) $('insp-delete').hidden = false;

  setValue(F.name, c.name);
  setValue(F.volRange, Math.round(c.volume * 100));
  setValue(F.vol, Math.round(c.volume * 100));
  setValue(F.fadeIn, c.fade_in);
  setValue(F.fadeOut, c.fade_out);
  setValue(F.preWait, c.pre_wait);
  setValue(F.duration, c.duration);
  setValue(F.loop, c.loop);
  setValue(F.auto, c.auto_continue);
  for (const input of Object.values(F)) input.disabled = !PERMS.edit;

  if (c.type === 'silence') {
    $('f-duration-label').textContent = 'Wait (s)';
    $('f-duration-hint').textContent = 'Stops everything, then waits this long. Useful before an auto-continue.';
  } else if (c.type === 'fadeout') {
    $('f-duration-label').textContent = 'Fade time (s)';
    $('f-duration-hint').textContent = 'Everything running fades to silence over this time.';
  }

  updateAuditionButtons();
  if (audioCue) {
    for (const id of ['t-set-in', 't-set-out', 't-restore']) $(id).hidden = !PERMS.edit;
    $('t-cut').hidden = !PERMS.edit;
    $('t-undo').hidden = !PERMS.edit;
    if (decoded.has(c.filename)) { drawTrim(); updateTrimReadout(); }
    else {
      updateTrimReadout();
      loadSource(c).then(() => { if (inspectedId === c.id) { drawTrim(); updateTrimReadout(); renderRow(c.id); } },
        () => { if (inspectedId === c.id) updateTrimReadout(); });
    }
  }
}

function changed(c, { live = false } = {}) {
  renderRow(c.id);
  if (c.id === inspectedId) $('insp-title').textContent = `${qLabel(c)} ${c.name}`;
  if (cues[standby]?.id === c.id) renderStandby();
  if (live) applyLiveVolume(c);
  scheduleSave();
}

function bindNumber(input, key, max) {
  input.addEventListener('change', () => {
    const c = cueById(inspectedId);
    if (!c) return;
    const v = parseFloat(input.value);
    if (!Number.isFinite(v) || v < 0) { input.value = c[key]; toast('Enter a number of seconds, 0 or more.', true); return; }
    c[key] = r4(Math.min(v, max));
    input.value = c[key];
    changed(c, { live: key === 'fade_out' });
  });
}
bindNumber(F.fadeIn, 'fade_in', 600);
bindNumber(F.fadeOut, 'fade_out', 600);
bindNumber(F.preWait, 'pre_wait', 3600);
bindNumber(F.duration, 'duration', 3600);

F.name.addEventListener('input', () => {
  const c = cueById(inspectedId);
  if (!c) return;
  c.name = F.name.value.slice(0, 100);
  changed(c);
});
F.name.addEventListener('change', () => {
  const c = cueById(inspectedId);
  if (c && !c.name.trim()) { c.name = 'Untitled cue'; F.name.value = c.name; changed(c); }
});

function setVolume(pct) {
  const c = cueById(inspectedId);
  if (!c || !Number.isFinite(pct)) return;
  pct = clamp(Math.round(pct), 0, 200);
  c.volume = pct / 100;
  F.vol.value = pct;
  F.volRange.value = pct;
  changed(c, { live: true });
}
F.volRange.addEventListener('input', () => setVolume(parseFloat(F.volRange.value)));
F.vol.addEventListener('change', () => setVolume(parseFloat(F.vol.value)));

F.loop.addEventListener('change', () => {
  const c = cueById(inspectedId);
  if (c) { c.loop = F.loop.checked; changed(c); }
});
F.auto.addEventListener('change', () => {
  const c = cueById(inspectedId);
  if (c) { c.auto_continue = F.auto.checked; changed(c); }
});

$('insp-audition').addEventListener('click', () => {
  const c = cueById(inspectedId);
  if (audition) stopAudition();
  else if (c) startAudition(c.trim_start);
});

$('insp-export').addEventListener('click', async () => {
  const c = cueById(inspectedId);
  if (!c) return;
  const btn = $('insp-export');
  btn.disabled = true;
  btn.textContent = 'Exporting…';
  try {
    const src = await loadSource(c);
    const blob = encodeWav(playable(c, src).buffer, c);
    downloadBlob(blob, safeFileName(`${CODE} ${qLabel(c)} ${c.name}`) + '.wav');
  } catch (e) {
    toast(`Couldn't export ${qLabel(c)}: ${e.message}.`, true);
  } finally {
    btn.disabled = false;
    btn.textContent = 'Export WAV';
  }
});

// 16-bit PCM WAV of the edited audio with volume and fades baked in (one pass if looping).
function encodeWav(buf, c) {
  const sr = buf.sampleRate;
  const chans = buf.numberOfChannels;
  const n = buf.length;
  const bytes = n * chans * 2;
  const ab = new ArrayBuffer(44 + bytes);
  const v = new DataView(ab);
  const text = (o, s) => { for (let i = 0; i < s.length; i++) v.setUint8(o + i, s.charCodeAt(i)); };
  text(0, 'RIFF'); v.setUint32(4, 36 + bytes, true); text(8, 'WAVE');
  text(12, 'fmt '); v.setUint32(16, 16, true); v.setUint16(20, 1, true); v.setUint16(22, chans, true);
  v.setUint32(24, sr, true); v.setUint32(28, sr * chans * 2, true); v.setUint16(32, chans * 2, true); v.setUint16(34, 16, true);
  text(36, 'data'); v.setUint32(40, bytes, true);

  const out = new Int16Array(ab, 44);
  const data = Array.from({ length: chans }, (_, i) => buf.getChannelData(i));
  const fi = Math.round(c.fade_in * sr);
  const fo = c.loop ? 0 : Math.round(c.fade_out * sr);
  let o = 0;
  for (let i = 0; i < n; i++) {
    let g = c.volume;
    if (i < fi) g *= i / fi;
    if (fo && i >= n - fo) g *= (n - i) / fo;
    for (let ch = 0; ch < chans; ch++) {
      const s = clamp(data[ch][i] * g, -1, 1);
      out[o++] = s < 0 ? s * 0x8000 : s * 0x7FFF;
    }
  }
  return new Blob([ab], { type: 'audio/wav' });
}

$('insp-duplicate')?.addEventListener('click', async () => {
  const c = cueById(inspectedId);
  if (!c) return;
  try {
    await flushSave();
    const d = await postJSON(`/cue/${encodeURIComponent(c.id)}/duplicate`);
    const copy = normaliseCue(d.cue);
    keepStandby(() => cues.splice(cueIndex(c.id) + 1, 0, copy));
    renderList();
    inspect(copy.id);
    toast(`Duplicated as ${qLabel(copy)}.`);
  } catch (e) {
    toast(`Couldn't duplicate ${qLabel(c)}: ${e.message}.`, true);
  }
});

$('insp-delete')?.addEventListener('click', async () => {
  const c = cueById(inspectedId);
  if (!c) return;
  const what = isAudio(c) ? ' Its audio file is removed from the server too.' : '';
  if (!confirm(`Delete ${qLabel(c)} "${c.name}"?${what} This can't be undone.`)) return;
  const i = cueIndex(c.id);
  try {
    await flushSave();
    await postJSON(`/cue/${encodeURIComponent(c.id)}/delete`);
  } catch (e) {
    toast(`Couldn't delete ${qLabel(c)}: ${e.message}.`, true);
    return;
  }
  stopCue(c.id, 0);
  keepStandby(() => cues.splice(i, 1));
  renders.delete(c.id);
  ed.undo.delete(c.id);
  renderList();
  inspect(cues[Math.min(i, cues.length - 1)]?.id ?? null);
  toast(`Deleted "${c.name}".`);
});

$('insp-replace')?.addEventListener('click', () => $('replace-file').click());
$('replace-file').addEventListener('change', async () => {
  const file = $('replace-file').files[0];
  const c = cueById(inspectedId);
  $('replace-file').value = '';
  if (!file || !c) return;
  if (!confirm(`Replace the audio in ${qLabel(c)} with "${file.name}"? Its trims and cuts will be cleared.`)) return;
  const fd = new FormData();
  fd.append('audio', file);
  const btn = $('insp-replace');
  btn.disabled = true;
  try {
    await flushSave();
    const d = await uploadWithProgress(`/cue/${encodeURIComponent(c.id)}/edit`, fd,
      p => { btn.textContent = `Uploading ${Math.round(p * 100)}%`; });
    forgetSource(c.filename);
    Object.assign(c, normaliseCue(d.cue));
    forgetSource(c.filename);
    renders.delete(c.id);
    ed.undo.delete(c.id);
    ed.marker = ed.sel = null;
    renderRow(c.id);
    renderInspector();
    toast(`Replaced the audio in ${qLabel(c)}.`);
  } catch (e) {
    toast(`Couldn't replace the audio: ${e.message}.`, true);
  } finally {
    btn.disabled = false;
    btn.textContent = 'Replace file';
  }
});

function setInspectorShown(show) {
  const insp = $('inspector');
  const btn = $('inspector-toggle');
  insp.hidden = !show;
  const label = show ? 'Hide inspector' : 'Show inspector';
  btn.title = label;
  btn.setAttribute('aria-label', label);
  btn.setAttribute('aria-expanded', String(show));
  try { localStorage.setItem('thorncue.inspector', show ? 'shown' : 'hidden'); } catch { /* storage off */ }
  if (show) { clampInspectorHeight(); renderInspector(); }
}

$('inspector-toggle').addEventListener('click', () => setInspectorShown($('inspector').hidden));

// Drag-to-resize bar on top of the inspector. The cue list keeps at least
// INSP_LIST_MIN px; the inspector can't shrink below INSP_MIN px.
const INSP_MIN = 80;
const INSP_LIST_MIN = 120;

function inspectorMaxHeight() {
  const insp = $('inspector');
  const cur = insp.hidden ? 0 : insp.offsetHeight;
  return Math.max(INSP_MIN, cur + $('cue-list-wrap').offsetHeight - INSP_LIST_MIN);
}

function setInspectorHeight(h, save = true) {
  const insp = $('inspector');
  h = Math.round(Math.min(Math.max(h, INSP_MIN), inspectorMaxHeight()));
  insp.style.height = h + 'px';
  insp.classList.add('is-sized');
  $('inspector-grip').setAttribute('aria-valuenow', String(h));
  if (save) { try { localStorage.setItem('thorncue.inspectorHeight', String(h)); } catch { /* storage off */ } }
}

function clampInspectorHeight() {
  const insp = $('inspector');
  if (insp.hidden || !insp.classList.contains('is-sized')) return;
  const h = parseFloat(insp.style.height);
  if (h > inspectorMaxHeight()) setInspectorHeight(h, false);
}

function resetInspectorHeight() {
  const insp = $('inspector');
  insp.style.height = '';
  insp.classList.remove('is-sized');
  $('inspector-grip').removeAttribute('aria-valuenow');
  try { localStorage.removeItem('thorncue.inspectorHeight'); } catch { /* storage off */ }
}

{
  const grip = $('inspector-grip');
  const bar = $('inspector-bar');
  let drag = null;
  grip.addEventListener('pointerdown', e => {
    if (e.button !== 0) return;
    const insp = $('inspector');
    drag = { y: e.clientY, h: insp.hidden ? 0 : insp.offsetHeight, id: e.pointerId };
    grip.setPointerCapture(e.pointerId);
    bar.classList.add('is-resizing');
    document.body.classList.add('is-resizing-inspector');
    e.preventDefault();
  });
  grip.addEventListener('pointermove', e => {
    if (!drag || e.pointerId !== drag.id) return;
    const h = drag.h + (drag.y - e.clientY);
    const insp = $('inspector');
    if (insp.hidden) {
      // Dragging the bar up far enough opens a hidden inspector.
      if (h < INSP_MIN / 2) return;
      setInspectorShown(true);
    }
    setInspectorHeight(h);
  });
  const end = e => {
    if (!drag || e.pointerId !== drag.id) return;
    drag = null;
    bar.classList.remove('is-resizing');
    document.body.classList.remove('is-resizing-inspector');
    drawTrim();
  };
  grip.addEventListener('pointerup', end);
  grip.addEventListener('pointercancel', end);
  grip.addEventListener('dblclick', () => { resetInspectorHeight(); if ($('inspector').hidden) setInspectorShown(true); });
  grip.addEventListener('keydown', e => {
    const insp = $('inspector');
    const step = e.shiftKey ? 80 : 20;
    if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
      e.preventDefault();
      if (insp.hidden) { if (e.key === 'ArrowUp') setInspectorShown(true); return; }
      setInspectorHeight(insp.offsetHeight + (e.key === 'ArrowUp' ? step : -step));
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      setInspectorShown(insp.hidden);
    } else if (e.key === 'Home') {
      e.preventDefault();
      resetInspectorHeight();
    }
  });
  window.addEventListener('resize', clampInspectorHeight);
}

// ---------------------------------------------------------------------------
// Trim editor
// ---------------------------------------------------------------------------

const wave = $('trim-wave');
const ed = {
  marker: null,   // source seconds: where "Audition from marker" starts
  sel: null,      // [a, b] source seconds
  drag: null,
  undo: new Map(), // cue id -> snapshots
};
let audition = null; // { cueId, node, startAt, segs, length }

const snapshot = c => ({ trim_start: c.trim_start, trim_end: c.trim_end, cuts: c.cuts.map(p => [...p]) });
const sameEdits = (a, b) => JSON.stringify(a) === JSON.stringify(b);

function pushUndo(c, before) {
  if (sameEdits(before, snapshot(c))) return;
  const stack = ed.undo.get(c.id) || [];
  stack.push(before);
  if (stack.length > 100) stack.shift();
  ed.undo.set(c.id, stack);
}

function editDone(c, before) {
  pushUndo(c, before);
  changed(c);
  drawTrim();
  updateTrimReadout();
}

function undoEdit() {
  const c = cueById(inspectedId);
  const stack = c && ed.undo.get(c.id);
  if (!stack?.length) return;
  Object.assign(c, stack.pop());
  changed(c);
  drawTrim();
  updateTrimReadout();
}

function inspectedSource() {
  const c = cueById(inspectedId);
  if (!c || !isAudio(c) || $('inspector').hidden) return {};
  return { c, src: decoded.get(c.filename) };
}

function xToTime(clientX, dur) {
  const r = wave.getBoundingClientRect();
  return clamp((clientX - r.left) / r.width, 0, 1) * dur;
}
const timeToCssX = (t, dur) => (t / dur) * wave.getBoundingClientRect().width;

function auditionSourceTime() {
  if (!audition || !ctx) return null;
  const t = ctx.currentTime - audition.startAt;
  if (t < 0) return audition.segs[0][0];
  if (t > audition.length) return null;
  return toSourceTime(audition.segs, t);
}

function drawTrim() {
  if ($('inspector').hidden) return;
  const { c, src } = inspectedSource();
  if (!c) return;
  const g = wave.getContext('2d');
  const { w, h, dpr } = sizeCanvas(wave);
  g.fillStyle = COLOR.bg;
  g.fillRect(0, 0, w, h);
  if (!src) {
    g.fillStyle = COLOR.wave;
    g.font = `${13 * dpr}px system-ui, sans-serif`;
    g.fillText(loadErrors.has(c.filename) ? `Can't load audio: ${loadErrors.get(c.filename)}` : 'Loading audio…', 8 * dpr, 20 * dpr);
    return;
  }
  const dur = src.duration;
  const segs = keptSegments(c, dur);
  const kept = t => segs.some(([s, e]) => t >= s && t <= e);
  const p = peaks(src, w);
  const mid = h / 2;
  for (let x = 0; x < w; x++) {
    g.fillStyle = kept(((x + 0.5) / w) * dur) ? COLOR.wave : COLOR.waveOff;
    const top = mid - Math.max(0.5, -p.min[x] * mid * 0.95);
    const bot = mid + Math.max(0.5, p.max[x] * mid * 0.95);
    g.fillRect(x, top, 1, bot - top);
  }
  const X = t => Math.round((t / dur) * w);
  if (ed.sel) {
    g.fillStyle = COLOR.selection;
    g.fillRect(X(ed.sel[0]), 0, Math.max(1, X(ed.sel[1]) - X(ed.sel[0])), h);
  }
  // In and out markers, with a tab at the top to grab.
  g.fillStyle = COLOR.marker;
  for (const [t, dir] of [[c.trim_start, 1], [outPoint(c, dur), -1]]) {
    const x = clamp(X(t), 0, w - 2 * dpr);
    g.fillRect(x, 0, 2 * dpr, h);
    g.fillRect(dir > 0 ? x : x - 8 * dpr + 2 * dpr, 0, 8 * dpr, 10 * dpr);
  }
  if (ed.marker != null) {
    g.fillStyle = COLOR.playhead;
    for (let y = 0; y < h; y += 6 * dpr) g.fillRect(X(ed.marker), y, 1 * dpr, 3 * dpr);
  }
  const ph = audition?.cueId === c.id ? auditionSourceTime() : null;
  if (ph != null) {
    g.fillStyle = COLOR.playhead;
    g.fillRect(X(ph), 0, 2 * dpr, h);
  }
}

function updateTrimReadout() {
  const { c, src } = inspectedSource();
  if (!c) return;
  const dur = src?.duration;
  $('t-in').textContent = fmtTime(c.trim_start, true);
  $('t-out').textContent = dur ? fmtTime(outPoint(c, dur), true) : (c.trim_end == null ? 'end' : fmtTime(c.trim_end, true));
  $('t-len').textContent = dur ? fmtTime(segmentsLength(keptSegments(c, dur)), true) : '…';
  $('t-sel-wrap').hidden = !ed.sel;
  if (ed.sel) $('t-sel').textContent = `${fmtTime(ed.sel[0], true)} to ${fmtTime(ed.sel[1], true)}`;
  $('t-cut').disabled = !ed.sel;
  $('t-undo').disabled = !(ed.undo.get(c.id)?.length);
  $('t-restore').disabled = !(c.trim_start > 0 || c.trim_end != null || c.cuts.length);

  const list = $('t-cuts');
  list.replaceChildren(...c.cuts.map(([a, b], i) => {
    const li = el('li', { className: 'num', textContent: `Cut ${fmtTime(a, true)} to ${fmtTime(b, true)}` });
    if (PERMS.edit) {
      const btn = el('button', { type: 'button', className: 'btn btn--small', textContent: 'Put back' });
      btn.addEventListener('click', () => {
        const before = snapshot(c);
        c.cuts.splice(i, 1);
        editDone(c, before);
      });
      li.append(btn);
    }
    return li;
  }));
}

function updateAuditionButtons() {
  const on = !!audition;
  $('insp-audition').textContent = on ? 'Stop audition' : 'Audition';
  $('insp-audition').setAttribute('aria-pressed', String(on));
  $('t-audition').textContent = on ? 'Stop audition' : 'Audition from marker';
  $('t-audition').setAttribute('aria-pressed', String(on));
}

// Plays through the show output, at the cue's volume, without fades or pre-wait.
async function startAudition(from) {
  const c = cueById(inspectedId);
  if (!c || !isAudio(c)) return;
  let src;
  try { src = await loadSource(c); }
  catch (e) { toast(`Can't audition ${qLabel(c)}: ${e.message}.`, true); return; }
  stopAudition();
  const ac = wake();
  const dur = src.duration;
  const out = outPoint(c, dur);
  from = clamp(from ?? c.trim_start, 0, dur);
  // From before the out point: hear the cue as edited. From after it: hear the rest of the file.
  const segs = from < out ? subtractCuts([[from, out]], c.cuts) : [[from, dur]];
  if (!segs.length) return;
  const node = ac.createBufferSource();
  const gain = ac.createGain();
  gain.gain.value = c.volume;
  node.connect(gain).connect(master);
  const startAt = ac.currentTime + 0.02;
  if (segs.length === 1) {
    node.buffer = src;
    node.start(startAt, segs[0][0], segs[0][1] - segs[0][0]);
  } else {
    node.buffer = renderSegments(src, segs);
    node.start(startAt);
  }
  const a = { cueId: c.id, node, startAt, segs, length: segmentsLength(segs) };
  node.onended = () => {
    if (audition === a) { audition = null; updateAuditionButtons(); drawTrim(); }
  };
  audition = a;
  updateAuditionButtons();
  startTicking();
}

function stopAudition() {
  if (!audition) return;
  const a = audition;
  audition = null;
  try { a.node.stop(); } catch { /* already stopped */ }
  updateAuditionButtons();
  drawTrim();
}

function markerOrPlayhead() {
  const ph = auditionSourceTime();
  if (ph != null) return ph;
  if (ed.marker != null) return ed.marker;
  toast('Click the waveform to place a marker, or audition and press the key while it plays.');
  return null;
}

function setIn() {
  const { c, src } = inspectedSource();
  if (!c || !src || !PERMS.edit) return;
  const t = markerOrPlayhead();
  if (t == null) return;
  if (t > outPoint(c, src.duration) - MIN_LEN) { toast('The in point has to be before the out point.', true); return; }
  const before = snapshot(c);
  c.trim_start = r4(t);
  editDone(c, before);
}

function setOut() {
  const { c, src } = inspectedSource();
  if (!c || !src || !PERMS.edit) return;
  const t = markerOrPlayhead();
  if (t == null) return;
  if (t < c.trim_start + MIN_LEN) { toast('The out point has to be after the in point.', true); return; }
  const before = snapshot(c);
  c.trim_end = t >= src.duration - 0.005 ? null : r4(t);
  editDone(c, before);
}

function cutSelection() {
  const { c, src } = inspectedSource();
  if (!c || !src || !ed.sel || !PERMS.edit) return;
  const dur = src.duration;
  const a = Math.max(ed.sel[0], c.trim_start);
  const b = Math.min(ed.sel[1], outPoint(c, dur));
  if (b - a < 0.01) { toast('The selection is outside the part that plays.', true); return; }
  const cuts = mergeCuts([...c.cuts, [a, b]]);
  if (segmentsLength(keptSegments({ ...c, cuts }, dur)) < MIN_LEN) { toast('That would cut the whole cue.', true); return; }
  const before = snapshot(c);
  c.cuts = cuts;
  ed.sel = null;
  editDone(c, before);
}

function restoreAll() {
  const c = cueById(inspectedId);
  if (!c) return;
  const before = snapshot(c);
  Object.assign(c, { trim_start: 0, trim_end: null, cuts: [] });
  editDone(c, before);
  toast(`${qLabel(c)} plays the full file again. Undo brings the edits back.`);
}

$('t-audition').addEventListener('click', () => (audition ? stopAudition() : startAudition(ed.marker)));
$('t-set-in').addEventListener('click', setIn);
$('t-set-out').addEventListener('click', setOut);
$('t-cut').addEventListener('click', cutSelection);
$('t-undo').addEventListener('click', undoEdit);
$('t-restore').addEventListener('click', restoreAll);

const HANDLE_PX = 8;
function handleAt(clientX, c, dur) {
  if (!PERMS.edit) return null;
  const x = clientX - wave.getBoundingClientRect().left;
  const dIn = Math.abs(x - timeToCssX(c.trim_start, dur));
  const dOut = Math.abs(x - timeToCssX(outPoint(c, dur), dur));
  if (Math.min(dIn, dOut) > HANDLE_PX) return null;
  return dIn <= dOut ? 'in' : 'out';
}

wave.addEventListener('pointerdown', e => {
  const { c, src } = inspectedSource();
  if (!c || !src) return;
  wave.setPointerCapture(e.pointerId);
  wave.focus();
  const dur = src.duration;
  const handle = handleAt(e.clientX, c, dur);
  ed.drag = handle
    ? { kind: handle, before: snapshot(c) }
    : { kind: 'select', anchor: xToTime(e.clientX, dur), startX: e.clientX, moved: false };
});

wave.addEventListener('pointermove', e => {
  const { c, src } = inspectedSource();
  if (!c || !src) return;
  const dur = src.duration;
  if (!ed.drag) {
    wave.style.cursor = handleAt(e.clientX, c, dur) ? 'ew-resize' : 'crosshair';
    return;
  }
  const t = xToTime(e.clientX, dur);
  if (ed.drag.kind === 'in') c.trim_start = r4(clamp(t, 0, outPoint(c, dur) - MIN_LEN));
  else if (ed.drag.kind === 'out') {
    const v = clamp(t, c.trim_start + MIN_LEN, dur);
    c.trim_end = v >= dur - 0.005 ? null : r4(v);
  } else if (Math.abs(e.clientX - ed.drag.startX) > 3) {
    ed.drag.moved = true;
    ed.sel = [Math.min(ed.drag.anchor, t), Math.max(ed.drag.anchor, t)];
  }
  drawTrim();
  updateTrimReadout();
});

function endDrag(e) {
  const { c, src } = inspectedSource();
  const d = ed.drag;
  ed.drag = null;
  if (!c || !src || !d) return;
  if (d.kind === 'select') {
    if (!d.moved) {
      ed.marker = xToTime(e.clientX, src.duration);
      ed.sel = null;
      if (audition) startAudition(ed.marker);
    }
    drawTrim();
    updateTrimReadout();
  } else {
    editDone(c, d.before);
  }
}
wave.addEventListener('pointerup', endDrag);
wave.addEventListener('pointercancel', endDrag);

window.addEventListener('resize', () => { drawTrim(); drawNowPlaying(); });

// ---------------------------------------------------------------------------
// Adding cues
// ---------------------------------------------------------------------------

function wireDialog(d) {
  d?.addEventListener('click', e => {
    if (e.target.closest('[data-close]') || e.target === d) d.close();
  });
}
document.querySelectorAll('dialog').forEach(wireDialog);

function addLocalCue(raw) {
  const c = normaliseCue(raw);
  // If standby was at the end of the list, the new cue becomes standby.
  keepStandby(() => cues.push(c));
  renderList();
  inspect(c.id);
  return c;
}

const addDialog = $('add-dialog');
let addFile = null;

function openAddDialog() {
  addFile = null;
  $('add-form').reset();
  $('drop-text').textContent = 'Drop an audio file here, or click to choose one.';
  $('add-preview').hidden = true;
  $('add-preview').removeAttribute('src');
  $('add-progress').hidden = true;
  $('add-error').hidden = true;
  $('add-submit').disabled = false;
  addDialog.showModal();
}

function chooseAddFile(file) {
  if (!file) return;
  addFile = file;
  $('drop-text').textContent = file.name;
  if (!$('add-name').value) $('add-name').value = file.name.replace(/\.[^.]+$/, '');
  const preview = $('add-preview');
  if (preview.src.startsWith('blob:')) URL.revokeObjectURL(preview.src);
  preview.src = URL.createObjectURL(file);
  preview.hidden = false;
}

if (addDialog) {
  $('add-cue-btn').addEventListener('click', openAddDialog);
  $('empty-add-btn')?.addEventListener('click', openAddDialog);
  $('add-file').addEventListener('change', () => chooseAddFile($('add-file').files[0]));
  const zone = $('drop-zone');
  zone.addEventListener('dragover', e => { e.preventDefault(); zone.classList.add('is-over'); });
  zone.addEventListener('dragleave', () => zone.classList.remove('is-over'));
  zone.addEventListener('drop', e => {
    e.preventDefault();
    zone.classList.remove('is-over');
    chooseAddFile(e.dataTransfer.files[0]);
  });
  addDialog.addEventListener('close', () => { $('add-preview').pause(); });

  $('add-form').addEventListener('submit', async e => {
    e.preventDefault();
    const err = $('add-error');
    if (!addFile) { err.textContent = 'Choose an audio file first.'; err.hidden = false; return; }
    err.hidden = true;
    const fd = new FormData();
    fd.append('audio', addFile);
    fd.append('name', $('add-name').value.trim() || addFile.name.replace(/\.[^.]+$/, ''));
    const progress = $('add-progress');
    progress.hidden = false;
    progress.value = 0;
    $('add-submit').disabled = true;
    try {
      await flushSave();
      const d = await uploadWithProgress('/cue/add', fd, p => { progress.value = p; });
      // Decode the local copy instead of downloading it again.
      const file = addFile;
      const p = file.arrayBuffer().then(ab => audio().decodeAudioData(ab))
        .then(buf => { decoded.set(d.cue.filename, buf); return buf; });
      sources.set(d.cue.filename, p);
      p.catch(() => sources.delete(d.cue.filename));
      addDialog.close();
      const c = addLocalCue(d.cue);
      p.then(() => { renderRow(c.id); if (inspectedId === c.id) renderInspector(); }, () => { });
      toast(`Added ${qLabel(c)} "${c.name}".`);
    } catch (ex) {
      err.textContent = `Upload failed: ${ex.message}.`;
      err.hidden = false;
    } finally {
      $('add-submit').disabled = false;
      progress.hidden = true;
    }
  });
}

async function addControlCue(type) {
  const fd = new FormData();
  fd.append('type', type);
  fd.append('name', type === 'silence' ? 'Silence' : 'Fade out all');
  fd.append('duration', '2');
  if (type === 'fadeout') fd.append('fade_out', '2');
  try {
    await flushSave();
    const d = await postForm('/cue/add', fd);
    addLocalCue(d.cue);
    F.duration.focus();
    F.duration.select();
  } catch (e) {
    toast(`Couldn't add the cue: ${e.message}.`, true);
  }
}
$('add-silence-btn')?.addEventListener('click', () => addControlCue('silence'));
$('add-fadeout-btn')?.addEventListener('click', () => addControlCue('fadeout'));

const urlDialog = $('url-dialog');
if (urlDialog) {
  $('import-url-btn').addEventListener('click', () => {
    $('url-form').reset();
    $('url-info').textContent = '';
    $('url-info').className = 'hint';
    $('url-name-field').hidden = true;
    $('url-add').hidden = true;
    $('url-check').hidden = false;
    urlDialog.showModal();
  });
  $('url-input').addEventListener('input', () => { $('url-add').hidden = true; $('url-check').hidden = false; });

  $('url-form').addEventListener('submit', async e => {
    e.preventDefault();
    const info = $('url-info');
    const btn = $('url-check');
    btn.disabled = true;
    info.className = 'hint';
    info.textContent = 'Checking the link…';
    try {
      const d = await request(`${API}/cue/import-url/info`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ url: $('url-input').value.trim() }),
      });
      info.textContent = `${d.title} (${fmtTime(d.duration)})`;
      info.className = 'hint status-ok';
      $('url-name').value = d.title;
      $('url-name-field').hidden = false;
      $('url-add').hidden = false;
      btn.hidden = true;
    } catch (ex) {
      info.textContent = ex.message.charAt(0).toUpperCase() + ex.message.slice(1);
      info.className = 'hint status-error';
    } finally {
      btn.disabled = false;
    }
  });

  $('url-add').addEventListener('click', async () => {
    const btn = $('url-add');
    btn.disabled = true;
    btn.textContent = 'Downloading…';
    try {
      await flushSave();
      const d = await postJSON('/cue/import-url', { url: $('url-input').value.trim(), name: $('url-name').value.trim() });
      urlDialog.close();
      const c = addLocalCue(d.cue);
      toast(`Added ${qLabel(c)} "${c.name}".`);
    } catch (ex) {
      $('url-info').textContent = `Download failed: ${ex.message}.`;
      $('url-info').className = 'hint status-error';
    } finally {
      btn.disabled = false;
      btn.textContent = 'Download and add';
    }
  });
}

// ---------------------------------------------------------------------------
// Show header
// ---------------------------------------------------------------------------

$('show-name').addEventListener('change', () => {
  const v = $('show-name').value.trim();
  if (!v) { $('show-name').value = SHOW.name; return; }
  SHOW.name = v;
  document.title = `${v} · ThornCue`;
  scheduleSave();
});
$('show-name').addEventListener('keydown', e => { if (e.key === 'Enter') $('show-name').blur(); });

$('copy-code').addEventListener('click', async () => {
  toast(await copyText(CODE) ? `Copied ${CODE}.` : `Show code: ${CODE}`);
});

$('master-vol').addEventListener('input', () => {
  masterVolume = parseFloat($('master-vol').value);
  $('master-vol-out').textContent = `${Math.round(masterVolume * 100)}%`;
  if (master && ctx) master.gain.setTargetAtTime(masterVolume, ctx.currentTime, 0.02);
});

$('go-btn').addEventListener('click', go);
$('stop-btn').addEventListener('click', () => stopAll());
$('pause-btn').addEventListener('click', togglePause);
$('hardstop-btn').addEventListener('click', hardStop);
$('top-btn').addEventListener('click', () => setStandby(0));
$('shortcuts-btn').addEventListener('click', () => $('shortcuts-dialog').showModal());

$('jump-form').addEventListener('submit', e => {
  e.preventDefault();
  if (jumpTo(parseInt($('jump-input').value, 10))) $('jump-dialog').close();
});

// Duplicate show
const dupeDialog = $('dupe-dialog');
if (dupeDialog) {
  let dupeTimer = null;
  const status = $('dupe-status');
  const setDupeStatus = (msg, kind) => { status.textContent = msg; status.className = 'hint' + (kind ? ` status-${kind}` : ''); };

  async function suggestDupeCode() {
    try {
      const d = await (await fetch('/api/suggest-code')).json();
      $('dupe-code').value = d.code;
      checkDupeCode();
    } catch { setDupeStatus("Couldn't get a suggestion. Type a code instead.", 'error'); }
  }

  function checkDupeCode() {
    const val = $('dupe-code').value.trim().toUpperCase();
    $('dupe-code').value = val;
    clearTimeout(dupeTimer);
    if (!val) { setDupeStatus('', ''); return; }
    dupeTimer = setTimeout(async () => {
      try {
        const d = await (await fetch(`/api/code-available/${encodeURIComponent(val)}`)).json();
        if (d.available) setDupeStatus(`${val} is free.`, 'ok');
        else setDupeStatus(d.reason === 'Invalid format' ? 'Use 3 to 20 letters, numbers and hyphens.' : `${val} is already in use.`, 'error');
      } catch { setDupeStatus("Couldn't check the code.", ''); }
    }, 300);
  }

  $('dupe-show-btn').addEventListener('click', () => {
    setDupeStatus('', '');
    dupeDialog.showModal();
    suggestDupeCode();
  });
  $('dupe-suggest').addEventListener('click', suggestDupeCode);
  $('dupe-code').addEventListener('input', checkDupeCode);
  $('dupe-form').addEventListener('submit', async e => {
    e.preventDefault();
    const btn = $('dupe-submit');
    btn.disabled = true;
    try {
      await flushSave();
      const d = await postJSON('/duplicate', {
        new_code: $('dupe-code').value.trim().toUpperCase(),
        new_name: $('dupe-name').value.trim(),
        author: SHOW.author,
      });
      location.href = `/show/${encodeURIComponent(d.new_code)}`;
    } catch (ex) {
      setDupeStatus(`Couldn't duplicate: ${ex.message}.`, 'error');
      btn.disabled = false;
    }
  });
}

// ---------------------------------------------------------------------------
// Sharing (owners only)
// ---------------------------------------------------------------------------

const shareDialog = $('share-dialog');
const PERM_NAMES = [['playback', 'Play'], ['upload', 'Add cues'], ['edit', 'Edit'], ['delete', 'Delete cues']];

function permChecks(row, onChange) {
  return PERM_NAMES.map(([key, label]) => {
    const box = el('input', { type: 'checkbox', checked: !!row[`perm_${key}`] });
    box.dataset.perm = key;
    box.addEventListener('change', onChange);
    return el('label', { className: 'check' }, box, ` ${label}`);
  });
}

function readPerms(container) {
  const out = {};
  for (const box of container.querySelectorAll('input[type=checkbox]')) out[box.dataset.perm || box.name] = box.checked;
  return out;
}

async function loadShares() {
  const list = $('shares-list');
  try {
    const rows = await request(`${API}/shares`);
    if (!rows.length) { list.replaceChildren(el('p', { className: 'muted', textContent: 'Not shared with anyone yet.' })); return; }
    list.replaceChildren(...rows.map(s => {
      const row = el('div', { className: 'grant-row' });
      const save = async () => {
        try { await postJSON('/shares/update', { user_id: s.user_id, ...readPerms(row) }); toast(`Updated ${s.username}.`); }
        catch (e) { toast(`Couldn't update ${s.username}: ${e.message}.`, true); }
      };
      const remove = el('button', { type: 'button', className: 'btn btn--small btn--danger', textContent: 'Remove' });
      remove.addEventListener('click', async () => {
        if (!confirm(`Remove ${s.username}'s access to this show?`)) return;
        try { await postJSON('/shares/remove', { user_id: s.user_id }); loadShares(); }
        catch (e) { toast(`Couldn't remove ${s.username}: ${e.message}.`, true); }
      });
      row.append(el('span', { className: 'grant-row__who', textContent: s.username }), ...permChecks(s, save), remove);
      return row;
    }));
  } catch (e) {
    list.replaceChildren(el('p', { className: 'status-error', textContent: `Couldn't load people: ${e.message}.` }));
  }
}

async function loadLinks() {
  const list = $('links-list');
  try {
    const rows = await request(`${API}/links`);
    if (!rows.length) { list.replaceChildren(el('p', { className: 'muted', textContent: 'No links yet.' })); return; }
    list.replaceChildren(...rows.map(l => {
      const row = el('div', { className: 'grant-row' });
      const save = async () => {
        try { await postJSON(`/links/${encodeURIComponent(l.token)}/update`, { label: l.label, ...readPerms(row) }); toast(`Updated "${l.label}".`); }
        catch (e) { toast(`Couldn't update the link: ${e.message}.`, true); }
      };
      const copy = el('button', { type: 'button', className: 'btn btn--small', textContent: 'Copy link' });
      copy.addEventListener('click', async () => {
        const url = `${location.origin}/join/${l.token}`;
        toast(await copyText(url) ? 'Link copied.' : url);
      });
      const revoke = el('button', { type: 'button', className: 'btn btn--small btn--danger', textContent: 'Revoke' });
      revoke.addEventListener('click', async () => {
        if (!confirm(`Revoke "${l.label}"? Anyone using it loses access.`)) return;
        try { await postJSON(`/links/${encodeURIComponent(l.token)}/revoke`); loadLinks(); }
        catch (e) { toast(`Couldn't revoke the link: ${e.message}.`, true); }
      });
      row.append(el('span', { className: 'grant-row__who', textContent: l.label }), ...permChecks(l, save), copy, revoke);
      return row;
    }));
  } catch (e) {
    list.replaceChildren(el('p', { className: 'status-error', textContent: `Couldn't load links: ${e.message}.` }));
  }
}

if (shareDialog) {
  $('share-btn').addEventListener('click', () => { loadShares(); loadLinks(); shareDialog.showModal(); });

  $('share-add-form').addEventListener('submit', async e => {
    e.preventDefault();
    const username = $('share-username').value.trim();
    try {
      await postJSON('/shares/add', { username, ...readPerms($('share-add-form')) });
      $('share-username').value = '';
      loadShares();
    } catch (ex) { toast(`Couldn't share with ${username}: ${ex.message}.`, true); }
  });

  $('link-add-form').addEventListener('submit', async e => {
    e.preventDefault();
    const label = $('link-label').value.trim() || 'Untitled link';
    try {
      const d = await postJSON('/links/create', { label, ...readPerms($('link-add-form')) });
      $('link-label').value = '';
      toast(await copyText(d.url) ? 'Link created and copied.' : `Link created: ${d.url}`);
      loadLinks();
    } catch (ex) { toast(`Couldn't create the link: ${ex.message}.`, true); }
  });

  $('transfer-form').addEventListener('submit', async e => {
    e.preventDefault();
    const username = $('transfer-username').value.trim();
    if (!confirm(`Make ${username} the owner of this show? You'll keep full access as a shared user.`)) return;
    try {
      await flushSave();
      await postJSON('/transfer', { username });
      location.reload();
    } catch (ex) { toast(`Couldn't transfer: ${ex.message}.`, true); }
  });
}

// ---------------------------------------------------------------------------
// Keyboard. Space is always GO outside text fields, as on a playback desk.
// ---------------------------------------------------------------------------

const isTyping = t => t.isContentEditable ||
  t.matches?.('textarea, select, input:not([type=checkbox]):not([type=range]):not([type=button]):not([type=file])');

document.addEventListener('keydown', e => {
  if (document.querySelector('dialog[open]')) return;
  const t = e.target;
  if (e.key === 'Escape') {
    if (isTyping(t)) t.blur();
    stopAll();
    return;
  }
  if (isTyping(t)) return;
  if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === 'z') {
    e.preventDefault();
    undoEdit();
    return;
  }
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  const onRange = t.matches?.('input[type=range]');

  switch (e.key) {
    case ' ':
      e.preventDefault();
      if (!e.repeat) go();
      break;
    case 'ArrowDown':
      if (onRange) return;
      e.preventDefault();
      moveStandby(1);
      break;
    case 'ArrowUp':
      if (onRange) return;
      e.preventDefault();
      moveStandby(-1);
      break;
    case 'p': case 'P': togglePause(); break;
    case 'j': case 'J':
      $('jump-form').reset();
      $('jump-dialog').showModal();
      break;
    case '?': $('shortcuts-dialog').showModal(); break;
    case 'i': case 'I': setIn(); break;
    case 'o': case 'O': setOut(); break;
    case 'x': case 'X': cutSelection(); break;
  }
});
// Stop Space from also clicking whichever button or checkbox has focus.
document.addEventListener('keyup', e => {
  if (e.key === ' ' && !isTyping(e.target) && !document.querySelector('dialog[open]')) e.preventDefault();
}, true);

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

try {
  const savedH = parseFloat(localStorage.getItem('thorncue.inspectorHeight'));
  if (savedH > 0) setInspectorHeight(savedH, false);
  if (localStorage.getItem('thorncue.inspector') === 'hidden') setInspectorShown(false);
} catch { /* storage off */ }

renderList();
renderInspector();
refreshTransport();
drawNowPlaying();

// Theme switched (settings in another tab, or the OS flipped while in System mode):
// repaint the waveforms only. No audio or focus changes, so playback and Space/Esc are untouched.
new MutationObserver(() => {
  readColors();
  drawNowPlaying();
  drawTrim();
}).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });

(async () => {
  for (const c of cues) {
    if (!isAudio(c)) continue;
    try { await loadSource(c); } catch { /* shown in the State column */ }
    renderRow(c.id);
    if (c.id === inspectedId) { drawTrim(); updateTrimReadout(); }
  }
})();
