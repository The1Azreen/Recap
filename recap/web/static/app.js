"use strict";

const $ = (s) => document.querySelector(s);
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const STYLE_NAMES = { general: "General", meeting: "Meeting", lecture: "Lecture", podcast: "Podcast" };

const state = {
  style: localGet("recap.style") || "general",
  file: null,
  item: null,      // {meta, transcript, summary}
  rows: [],        // transcript row elements, parallel to item.transcript.segments
  active: -1,
  media: null,     // <video>/<audio>
  yt: null,        // YouTube iframe
  busy: false,
};

function localGet(k) { try { return localStorage.getItem(k); } catch { return null; } }
function localSet(k, v) { try { localStorage.setItem(k, v); } catch { /* private mode */ } }

function fmt(sec) {
  sec = Math.max(0, Math.floor(sec || 0));
  const h = Math.floor(sec / 3600), m = Math.floor(sec / 60) % 60, s = sec % 60;
  const p = (n) => String(n).padStart(2, "0");
  return h ? `${h}:${p(m)}:${p(s)}` : `${p(m)}:${p(s)}`;
}
function parseTs(str) {
  return str.split(":").map(Number).reduce((a, n) => a * 60 + n, 0);
}

async function api(path, opts = {}) {
  const r = await fetch(path, opts);
  if (!r.ok) {
    let msg = r.statusText;
    try { msg = (await r.json()).detail || msg; } catch { /* not json */ }
    throw new Error(msg);
  }
  return r.json();
}

/* ---------------------------------------------------------------- taskbar */
function status(msg, { frac = null, busy = false, err = false } = {}) {
  const el = $("#status");
  el.textContent = msg;
  el.title = msg;
  el.classList.toggle("err", err);
  $("#status-dot").className = "slot status-dot" + (busy ? " busy" : err ? " err" : "");
  const bar = $("#bar");
  bar.classList.toggle("indet", busy && frac == null);
  bar.classList.toggle("on", frac != null);
  const pct = frac == null ? (busy ? null : 0) : Math.round(frac * 100);
  if (pct != null) {
    $("#bar-fill").style.width = pct + "%";
    $("#bar-knob").style.left = pct + "%";
  }
  $("#pct").textContent = frac != null ? pct + "%" : "";
}

/* ---------------------------------------------------------------- style tabs */
function setStyle(s) {
  state.style = s;
  localSet("recap.style", s);
  document.querySelectorAll(".style-tab").forEach((b) => b.classList.toggle("on", b.dataset.style === s));
  $("#style-name").textContent = STYLE_NAMES[s] || s;
  const re = $("#resum");
  if (re) re.textContent = `Summarize as ${STYLE_NAMES[s]}`;
}
document.querySelectorAll(".style-tab").forEach((b) => b.addEventListener("click", () => setStyle(b.dataset.style)));

/* ---------------------------------------------------------------- new recap form */
function setFile(f) {
  state.file = f;
  const drop = $("#drop");
  drop.classList.toggle("has-file", !!f);
  $("#drop-label").textContent = f ? f.name : "Drop audio / video here";
  if (f) $("#url").value = "";
}
$("#file").addEventListener("change", (e) => setFile(e.target.files[0] || null));
$("#url").addEventListener("input", () => { if ($("#url").value) setFile(null); });
$("#btn-open").addEventListener("click", () => $("#file").click());
$("#btn-new").addEventListener("click", () => { setFile(null); $("#url").value = ""; $("#url").focus(); });

let dragDepth = 0;
window.addEventListener("dragenter", (e) => { if (e.dataTransfer?.types.includes("Files")) { dragDepth++; $("#dropveil").classList.add("on"); } });
window.addEventListener("dragleave", () => { if (--dragDepth <= 0) { dragDepth = 0; $("#dropveil").classList.remove("on"); } });
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  $("#dropveil").classList.remove("on");
  const f = e.dataTransfer?.files?.[0];
  if (f) setFile(f);
});

$("#new-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  if (state.busy) return;
  const url = $("#url").value.trim();
  if (!url && !state.file) { status("Paste a link or choose a file first", { err: true }); return; }
  const fd = new FormData();
  if (state.file) fd.append("file", state.file); else fd.append("url", url);
  fd.append("style", state.style);
  fd.append("summary", $("#opt-summary").checked);
  fd.append("transcribe", $("#opt-transcribe").checked);
  setBusy(true);
  status(state.file ? "Uploading…" : "Starting…", { busy: true });
  try {
    const job = await api("/api/jobs", { method: "POST", body: fd });
    poll(job.id);
  } catch (err) {
    setBusy(false);
    status(err.message, { err: true });
  }
});

function setBusy(b) {
  state.busy = b;
  $("#go").disabled = b;
  $("#go").innerHTML = b ? "Working…" : "&#9654; Recap it";
}

async function poll(id) {
  let job;
  try { job = await api(`/api/jobs/${id}`); } catch (err) { setBusy(false); status(err.message, { err: true }); return; }
  if (job.status === "done") {
    setBusy(false);
    setFile(null);
    $("#url").value = "";
    status("Done", { frac: 1 });
    await loadLibrary();
    openItem(job.item_id);
    return;
  }
  if (job.status === "error") {
    setBusy(false);
    status(job.message, { err: true });
    return;
  }
  status(job.message || job.stage, { frac: job.progress, busy: true });
  setTimeout(() => poll(id), 600);
}

/* ---------------------------------------------------------------- library */
let libItems = [];
async function loadLibrary() {
  libItems = await api("/api/library");
  const total = libItems.reduce((a, m) => a + (m.duration || 0), 0);
  $("#count").textContent = `${libItems.length} recap${libItems.length === 1 ? "" : "s"} · ${fmt(total)}`;
  if (!$("#lib-search").value.trim()) renderLibrary();
}

function methodShort(m) {
  if (!m.method) return m.source_type;
  if (m.method.startsWith("YouTube")) return "YouTube captions";
  return m.method.replace(/\s*\(.*\)$/, "") + (m.method.includes("Vulkan") ? " · GPU" : "");
}

function renderLibrary() {
  const ul = $("#lib");
  if (!libItems.length) { ul.innerHTML = `<li class="lib-empty">Nothing here yet.</li>`; return; }
  ul.innerHTML = libItems.map((m) => `
    <li data-id="${esc(m.id)}" class="${state.item?.meta.id === m.id ? "on" : ""}">
      <span class="t">${esc(m.title)}</span>
      <span class="s">${fmt(m.duration)} · ${esc(methodShort(m))}${m.style ? " · " + esc(STYLE_NAMES[m.style] || m.style) : ""}</span>
      <button class="x" title="Delete" data-del="${esc(m.id)}">×</button>
    </li>`).join("");
}

$("#lib").addEventListener("click", async (e) => {
  const del = e.target.closest("[data-del]");
  if (del) {
    e.stopPropagation();
    const m = libItems.find((x) => x.id === del.dataset.del);
    if (!confirm(`Delete “${m?.title}”? This removes its transcript, summary and saved media.`)) return;
    await api(`/api/library/${del.dataset.del}`, { method: "DELETE" });
    if (state.item?.meta.id === del.dataset.del) clearItem();
    loadLibrary();
    return;
  }
  const li = e.target.closest("li[data-id]");
  if (li) openItem(li.dataset.id, li.dataset.t ? Number(li.dataset.t) : null);
});

let searchTimer;
$("#lib-search").addEventListener("input", () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(runSearch, 220);
});
async function runSearch() {
  const q = $("#lib-search").value.trim();
  if (!q) { renderLibrary(); return; }
  const hits = await api(`/api/search?q=${encodeURIComponent(q)}`);
  const re = new RegExp(q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "gi");
  $("#lib").innerHTML = hits.length ? hits.map((h) => `
    <li data-id="${esc(h.id)}" data-t="${h.start}">
      <span class="t">${esc(h.title)}</span>
      ${h.text ? `<span class="hit"><span class="s" style="display:inline">${fmt(h.start)}</span> ${esc(h.text).replace(re, (m) => `<mark>${m}</mark>`)}</span>` : ""}
    </li>`).join("") : `<li class="lib-empty">No matches.</li>`;
}

/* ---------------------------------------------------------------- open a recap */
function clearItem() {
  state.item = null;
  location.hash = "";
  $("#summary").innerHTML = `<div class="empty"><div class="empty-art"></div><p>Paste a link or drop a file<br>to make your first recap<span class="caret">_</span></p></div>`;
  $("#transcript").innerHTML = "";
  $("#win-player").hidden = true;
  $("#player").innerHTML = "";
  $("#question").disabled = $("#ask-btn").disabled = true;
}

async function openItem(id, seekTo = null) {
  let data;
  try { data = await api(`/api/library/${id}`); } catch (err) { status(err.message, { err: true }); return; }
  state.item = data;
  location.hash = id;
  renderSummary();
  renderTranscript();
  renderPlayer();
  renderLibraryMarks();
  $("#question").disabled = $("#ask-btn").disabled = false;
  $("#answer").innerHTML = `<span class="dim">Ask anything about “${esc(data.meta.title)}”.</span>`;
  if (seekTo != null) setTimeout(() => seek(seekTo), 400);
}

function renderLibraryMarks() {
  document.querySelectorAll("#lib li[data-id]").forEach((li) => li.classList.toggle("on", li.dataset.id === state.item?.meta.id));
}

function tsBtn(sec) { return `<button class="ts" data-seek="${sec}">${fmt(sec)}</button>`; }

function renderSummary() {
  const { meta, summary: s, transcript: t } = state.item;
  const src = meta.source.startsWith("http")
    ? `<a href="${esc(meta.source)}" target="_blank" rel="noopener">${esc(meta.source_type === "youtube" ? "YouTube" : "link")}</a>`
    : esc(meta.source.split(/[\\/]/).pop());
  const head = `
    <h2 class="sum-title">${esc(meta.title)}</h2>
    <div class="sum-meta">${src} · ${fmt(t.duration)} · ${esc(meta.method)}${meta.language ? " · " + esc(meta.language) : ""}</div>
    <div class="toolbar">
      <a class="chip" href="/api/library/${meta.id}/export/md">.md</a>
      <a class="chip" href="/api/library/${meta.id}/export/srt">.srt</a>
      <a class="chip" href="/api/library/${meta.id}/export/txt">.txt</a>
      <a class="chip" href="/api/library/${meta.id}/export/json">.json</a>
      <button class="chip" id="copy-md">Copy Markdown</button>
      <span class="sep"></span>
      <button class="chip" id="resum">Summarize as ${STYLE_NAMES[state.style]}</button>
    </div>`;
  let body;
  if (!s) {
    body = `<p class="nosum">No summary yet. Pick a style above and press “Summarize”.</p>`;
  } else {
    body = `
      <div class="h">TL;DR</div><p class="tldr">${esc(s.tldr)}</p>
      ${s.key_points?.length ? `<div class="h">Key points</div><ul class="pts">${s.key_points.map((p) => `<li>${linkTimes(esc(p))}</li>`).join("")}</ul>` : ""}
      ${s.action_items?.length ? `<div class="h">Action items</div><ul class="pts todo">${s.action_items.map((p) => `<li>${linkTimes(esc(p))}</li>`).join("")}</ul>` : ""}
      ${s.chapters?.length ? `<div class="h">Chapters</div><div class="chap">${s.chapters.map((c) => `${tsBtn(c.start)}<div><b>${esc(c.title)}</b><span class="cs">${esc(c.summary)}</span></div>`).join("")}</div>` : ""}`;
  }
  $("#summary").innerHTML = head + body;
  $("#summary").scrollTop = 0;
  $("#copy-md").addEventListener("click", copyMarkdown);
  $("#resum").addEventListener("click", resummarize);
}

async function copyMarkdown() {
  const r = await fetch(`/api/library/${state.item.meta.id}/export/md`);
  try { await navigator.clipboard.writeText(await r.text()); status("Markdown copied"); }
  catch { status("Couldn't reach the clipboard", { err: true }); }
}

async function resummarize() {
  if (state.busy) return;
  const id = state.item.meta.id;
  setBusy(true);
  status(`Summarizing as ${STYLE_NAMES[state.style]} with Claude…`, { busy: true });
  try {
    const r = await api(`/api/library/${id}/summarize`, {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ style: state.style }),
    });
    if (state.item?.meta.id === id) {
      state.item.summary = r.summary;
      state.item.meta = r.meta;
      renderSummary();
    }
    status("Summary updated", { frac: 1 });
    loadLibrary();
  } catch (err) {
    status(err.message, { err: true });
  } finally { setBusy(false); }
}

function renderTranscript() {
  const segs = state.item.transcript.segments;
  const box = $("#transcript");
  if (!segs.length) { box.innerHTML = `<div class="none">No speech found.</div>`; state.rows = []; return; }
  box.innerHTML = segs.map((s, i) => `<div class="row" data-i="${i}"><span class="tm">${fmt(s.start)}</span><span class="tx">${s.speaker ? `<span class="sp">${esc(s.speaker)}:</span> ` : ""}${esc(s.text)}</span></div>`).join("");
  state.rows = [...box.children];
  state.active = -1;
  box.scrollTop = 0;
  $("#tr-filter").value = "";
}

$("#transcript").addEventListener("click", (e) => {
  const row = e.target.closest(".row");
  if (row) seek(state.item.transcript.segments[+row.dataset.i].start);
});

$("#tr-filter").addEventListener("input", () => {
  if (!state.item) return;
  const q = $("#tr-filter").value.trim().toLowerCase();
  const re = q ? new RegExp(q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "gi") : null;
  state.item.transcript.segments.forEach((s, i) => {
    const row = state.rows[i];
    const hit = !q || s.text.toLowerCase().includes(q);
    row.hidden = !hit;
    row.querySelector(".tx").innerHTML = (s.speaker ? `<span class="sp">${esc(s.speaker)}:</span> ` : "") +
      (re ? esc(s.text).replace(re, (m) => `<mark>${m}</mark>`) : esc(s.text));
  });
});

/* ---------------------------------------------------------------- player + sync */
function renderPlayer() {
  const { meta } = state.item;
  const box = $("#player");
  state.media = state.yt = null;
  box.innerHTML = "";
  if (meta.video_id) {
    const f = document.createElement("iframe");
    f.src = `https://www.youtube-nocookie.com/embed/${encodeURIComponent(meta.video_id)}?enablejsapi=1&rel=0&origin=${encodeURIComponent(location.origin)}`;
    f.allow = "autoplay; encrypted-media; picture-in-picture; fullscreen";
    f.title = meta.title;
    f.addEventListener("load", () => {
      // Ask the embed to stream its state (currentTime) to us via postMessage.
      f.contentWindow.postMessage(JSON.stringify({ event: "listening", id: 1 }), "*");
    });
    box.appendChild(f);
    state.yt = f;
  } else if (meta.media) {
    const isVideo = meta.source_type === "video";
    const el = document.createElement(isVideo ? "video" : "audio");
    el.controls = true;
    el.preload = "metadata";
    el.src = `/api/library/${meta.id}/media`;
    el.addEventListener("timeupdate", () => syncTo(el.currentTime));
    box.appendChild(el);
    state.media = el;
  }
  $("#win-player").hidden = !(state.yt || state.media);
}

window.addEventListener("message", (e) => {
  if (!state.yt || e.source !== state.yt.contentWindow) return;
  let d;
  try { d = typeof e.data === "string" ? JSON.parse(e.data) : e.data; } catch { return; }
  const t = d?.info?.currentTime;
  if (typeof t === "number") syncTo(t);
});

function seek(sec) {
  if (state.media) {
    state.media.currentTime = sec;
    state.media.play().catch(() => {});
  } else if (state.yt) {
    const w = state.yt.contentWindow;
    w.postMessage(JSON.stringify({ event: "command", func: "seekTo", args: [sec, true] }), "*");
    w.postMessage(JSON.stringify({ event: "command", func: "playVideo", args: [] }), "*");
  }
  syncTo(sec, true);
}

function syncTo(t, force = false) {
  const segs = state.item?.transcript.segments;
  if (!segs?.length) return;
  let lo = 0, hi = segs.length - 1, idx = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (segs[mid].start <= t + 0.05) { idx = mid; lo = mid + 1; } else hi = mid - 1;
  }
  if (idx === state.active && !force) return;
  state.rows[state.active]?.classList.remove("on");
  state.active = idx;
  const row = state.rows[idx];
  if (!row) return;
  row.classList.add("on");
  if ($("#follow").checked || force) {
    const box = $("#transcript");
    const top = row.offsetTop - box.offsetTop - box.clientHeight / 3;
    box.scrollTo({ top, behavior: "smooth" });
  }
}

document.addEventListener("click", (e) => {
  const b = e.target.closest("[data-seek]");
  if (b) seek(Number(b.dataset.seek));
});

/* ---------------------------------------------------------------- ask */
function linkTimes(html) {
  return html.replace(/\[((?:\d{1,2}:)?\d{1,2}:\d{2})\]/g, (m, ts) => tsBtn(parseTs(ts)));
}

$("#ask-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const q = $("#question").value.trim();
  if (!q || !state.item) return;
  const id = state.item.meta.id;
  $("#ask-btn").disabled = true;
  $("#answer").innerHTML = `<span class="q">&gt; ${esc(q)}</span><span class="dim">Thinking<span class="caret">_</span></span>`;
  status("Asking Claude…", { busy: true });
  try {
    const r = await api(`/api/library/${id}/ask`, {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ question: q }),
    });
    if (state.item?.meta.id === id) {
      $("#answer").innerHTML = `<span class="q">&gt; ${esc(q)}</span>${linkTimes(esc(r.answer))}`;
      $("#question").value = "";
    }
    status("Answered");
  } catch (err) {
    $("#answer").innerHTML = `<span class="q">&gt; ${esc(q)}</span><span class="dim">${esc(err.message)}</span>`;
    status(err.message, { err: true });
  } finally { $("#ask-btn").disabled = false; }
});

/* ---------------------------------------------------------------- boot */
(async function boot() {
  setStyle(state.style);
  try {
    const sys = await api("/api/system");
    const chip = $("#engine-chip");
    chip.textContent = sys.gpu ? "GPU: Vulkan" : sys.engine.includes("CPU") ? "CPU only" : "No transcriber";
    chip.title = `Transcription: ${sys.engine}\nSummaries: ${sys.claude_model}${sys.has_key ? "" : " (ANTHROPIC_API_KEY not set)"}`;
    chip.classList.toggle("gpu", sys.gpu);
    if (!sys.has_key) {
      status("ANTHROPIC_API_KEY isn't set: transcripts work, summaries won't", { err: true });
    }
  } catch { /* server still starting */ }
  await loadLibrary();
  const id = location.hash.slice(1);
  if (id && libItems.some((m) => m.id === id)) openItem(id);
})();
