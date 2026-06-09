import { invoke, Channel } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Tipos (espejo de los structs de Rust)
// ---------------------------------------------------------------------------
interface AppConfig {
  canvas_url: string;
  canvas_token: string;
  download_dir: string;
  token_created: string;
  ollama_url: string;
  classify_model: string;
  embed_model: string;
  summary_provider: string;
  summary_model: string;
  dynamic_theme: boolean;
}
interface Course { id: number; name: string; code: string; term: string; concluded: boolean; }
interface Assignment {
  id: number; course_id: number; course_name: string; name: string;
  due_at: string | null; points: number | null; html_url: string;
  submitted: boolean; state: string; user_status: string;
}
interface CanvasFile {
  id: number; course_id: number; course_name: string; folder: string;
  filename: string; url: string; size: number; content_type: string;
  local_path: string; category: string; summary: string; embedding: number[];
  category_manual?: boolean; summary_pdf?: string;
}
interface CourseSummary { course_id: number; course_name: string; category: string; summary: string; }
interface CalEvent { id: string; title: string; date: string; course_name: string; source: string; }
interface AppState { courses: Course[]; assignments: Assignment[]; files: CanvasFile[]; course_summaries: CourseSummary[]; events: CalEvent[]; last_sync: string; notified: string[]; }
interface ProviderStatus { openrouter: boolean; gemini: boolean; }
interface SearchHit { file_id: number; filename: string; course_name: string; category: string; score: number; }

type Progress =
  | { type: "step"; phase: string; current: number; total: number; message: string }
  | { type: "log"; message: string }
  | { type: "done"; message: string }
  | { type: "error"; message: string };

// ---------------------------------------------------------------------------
// Estado global del frontend
// ---------------------------------------------------------------------------
let config: AppConfig = {
  canvas_url: "", canvas_token: "", download_dir: "", token_created: "", ollama_url: "",
  classify_model: "", embed_model: "", summary_provider: "gemini", summary_model: "", dynamic_theme: false,
};

// Categorías cuyo material se resume (debe coincidir con ai::is_summarizable en Rust).
const RESUMIBLES = ["Clase", "Ayudantía", "Cápsula", "Laboratorio", "Guía/Ejercicios", "Tarea", "Prueba/Control", "Lectura/Paper"];
const TOKEN_DAYS = 120;

function concludedIds(): Set<number> {
  return new Set(state.courses.filter((c) => c.concluded).map((c) => c.id));
}

// Formatea el periodo a "Primer/Segundo semestre 20XX" (o TAV).
function termLabel(term: string): string {
  const t = (term || "").trim();
  if (!t) return "";
  const l = t.toLowerCase();
  const year = (l.match(/\d{4}/) || [""])[0];
  let sem = "";
  if (l.includes("primer") || l.includes("-1") || l.includes(" 1") || l.includes("/1")) sem = "Primer semestre";
  else if (l.includes("segundo") || l.includes("-2") || l.includes(" 2") || l.includes("/2")) sem = "Segundo semestre";
  else if (l.includes("tav") || l.includes("verano")) sem = "TAV";
  if (sem && year) return `${sem} ${year}`;
  return sem || year || t;
}

// Nombres de ramo que aparecen en más de un periodo (repetidos).
function dupCourseNames(): Set<string> {
  const counts: Record<string, number> = {};
  for (const c of state.courses) counts[c.name] = (counts[c.name] || 0) + 1;
  return new Set(Object.keys(counts).filter((n) => counts[n] > 1));
}

// Nombre a mostrar: añade el semestre si el ramo está repetido.
function courseDisplay(id: number, fallback: string): string {
  const c = state.courses.find((x) => x.id === id);
  if (!c) return fallback;
  if (dupCourseNames().has(c.name)) return `${c.name} · ${termLabel(c.term) || "#" + c.id}`;
  return c.name;
}
function tokenDaysLeft(): number | null {
  if (!config.canvas_token || !config.token_created) return null;
  const created = new Date(config.token_created).getTime();
  if (isNaN(created)) return null;
  const elapsed = Math.floor((Date.now() - created) / 86400000);
  return TOKEN_DAYS - elapsed;
}
let state: AppState = { courses: [], assignments: [], files: [], course_summaries: [], events: [], last_sync: "", notified: [] };
let providers: ProviderStatus = { openrouter: false, gemini: false };
let allCategories: string[] = [];
let view = "dashboard";
let courseId = 0;       // ramo abierto en la vista de detalle (maestro-detalle)
let courseSel = "";     // selección dentro del ramo: "gen:<cat>" o "file:<id>"
let busy = false;
let lastLive = 0;

const app = document.getElementById("app")!;

// ---------------------------------------------------------------------------
// Utilidades
// ---------------------------------------------------------------------------
const esc = (s: string) =>
  (s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

function fmtSize(n: number): string {
  if (!n) return "";
  const u = ["B", "KB", "MB", "GB"];
  let i = 0; let v = n;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(v < 10 && i > 0 ? 1 : 0)} ${u[i]}`;
}

function dueInfo(due: string | null): { label: string; cls: string; ts: number } {
  if (!due) return { label: "Sin fecha", cls: "", ts: Number.MAX_SAFE_INTEGER };
  const d = new Date(due).getTime();
  const now = Date.now();
  const days = Math.round((d - now) / 86400000);
  const fecha = new Date(due).toLocaleDateString("es-CL", { day: "2-digit", month: "short" });
  if (days < 0) return { label: `Atrasada · ${fecha}`, cls: "red", ts: d };
  if (days === 0) return { label: `Hoy · ${fecha}`, cls: "red", ts: d };
  if (days === 1) return { label: `Mañana · ${fecha}`, cls: "yellow", ts: d };
  if (days <= 7) return { label: `En ${days} días · ${fecha}`, cls: "yellow", ts: d };
  return { label: fecha, cls: "blue", ts: d };
}

function pendingAssignments(): Assignment[] {
  const past = concludedIds();
  return state.assignments
    .filter((a) => !a.submitted)
    .filter((a) => a.user_status !== "done" && a.user_status !== "dismissed")
    .filter((a) => !past.has(a.course_id)) // cursos pasados no cuentan como pendientes
    .filter((a) => a.due_at !== null) // tareas sin fecha suelen ser plantillas
    .sort((a, b) => dueInfo(a.due_at).ts - dueInfo(b.due_at).ts);
}

function catColor(cat: string): string {
  const m: Record<string, string> = {
    "Tarea": "rust", "Prueba/Control": "gold", "Guía/Ejercicios": "amber",
    "Clase": "sage", "Cápsula": "sage", "Lectura/Paper": "sage", "Laboratorio": "amber",
    "Ayudantía": "amber", "Programa/Syllabus": "gold", "Administrativo": "", "Otro": "",
  };
  return m[cat] ?? "";
}

// Mini-render de markdown (encabezados, listas, negrita, código).
function renderMd(src: string): string {
  const lines = esc(src).split("\n");
  let html = ""; let inList = false;
  const inline = (t: string) =>
    t.replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>").replace(/`(.+?)`/g, "<code>$1</code>");
  for (const raw of lines) {
    const l = raw.trim();
    if (l.startsWith("## ")) { if (inList) { html += "</ul>"; inList = false; } html += `<h2>${inline(l.slice(3))}</h2>`; }
    else if (l.startsWith("### ")) { if (inList) { html += "</ul>"; inList = false; } html += `<h3>${inline(l.slice(4))}</h3>`; }
    else if (l.startsWith("#### ")) { if (inList) { html += "</ul>"; inList = false; } html += `<h3>${inline(l.slice(5))}</h3>`; }
    else if (/^[-*] /.test(l)) { if (!inList) { html += "<ul>"; inList = true; } html += `<li>${inline(l.slice(2))}</li>`; }
    else if (/^\d+\. /.test(l)) { if (!inList) { html += "<ul>"; inList = true; } html += `<li>${inline(l.replace(/^\d+\. /, ""))}</li>`; }
    else if (l === "") { if (inList) { html += "</ul>"; inList = false; } }
    else { if (inList) { html += "</ul>"; inList = false; } html += `<p>${inline(l)}</p>`; }
  }
  if (inList) html += "</ul>";
  return html;
}

// ---------------------------------------------------------------------------
// Toast + progreso
// ---------------------------------------------------------------------------
function toast(msg: string, kind: "ok" | "err" = "ok") {
  const t = document.createElement("div");
  t.className = `toast ${kind}`;
  t.textContent = msg;
  document.body.appendChild(t);
  requestAnimationFrame(() => t.classList.add("show"));
  setTimeout(() => { t.classList.remove("show"); setTimeout(() => t.remove(), 320); }, 3600);
}

function progressEls() {
  let wrap = document.querySelector(".progress-wrap") as HTMLElement | null;
  if (!wrap) {
    wrap = document.createElement("div");
    wrap.className = "progress-wrap";
    wrap.innerHTML = `<div class="progress-head"><span class="ph"></span><span class="msg"></span><span class="pct"></span></div><div class="bar"><span></span></div>`;
    document.body.appendChild(wrap);
  }
  return {
    wrap,
    ph: wrap.querySelector(".ph") as HTMLElement,
    msg: wrap.querySelector(".msg") as HTMLElement,
    pct: wrap.querySelector(".pct") as HTMLElement,
    bar: wrap.querySelector(".bar > span") as HTMLElement,
  };
}

const phaseLabel: Record<string, string> = {
  sync: "Sincronizando", download: "Descargando", classify: "Clasificando (IA local)",
  summarize: "Resumiendo (IA cloud)", general: "Resumen general (IA cloud)",
};

// Ejecuta un comando con Channel<Progress> y muestra la barra.
function runProgress(cmd: string, args: Record<string, unknown> = {}): Promise<void> {
  return new Promise((resolve) => {
    busy = true; refreshToolbar();
    const p = progressEls();
    p.wrap.classList.add("show");
    p.ph.textContent = "Iniciando…"; p.msg.textContent = ""; p.pct.textContent = ""; p.bar.style.width = "0%";
    const channel = new Channel<Progress>();
    channel.onmessage = (m) => {
      if (m.type === "step") {
        p.ph.textContent = phaseLabel[m.phase] ?? m.phase;
        p.msg.textContent = m.message;
        const pct = m.total ? Math.round((m.current / m.total) * 100) : 0;
        p.pct.textContent = `${m.current}/${m.total}`;
        p.bar.style.width = `${pct}%`;
        // Actualización EN VIVO: refresca la app cada ~2.5 s para ver el avance.
        const now = Date.now();
        if (now - lastLive > 2500) {
          lastLive = now;
          load().then(() => { if (busy) render(); });
        }
      } else if (m.type === "log") {
        p.msg.textContent = m.message;
      } else if (m.type === "done") {
        p.bar.style.width = "100%";
        finish(); toast(m.message, "ok");
      } else if (m.type === "error") {
        finish(); toast(m.message, "err");
      }
    };
    const finish = () => {
      busy = false; refreshToolbar();
      setTimeout(() => p.wrap.classList.remove("show"), 700);
      load().then(() => { render(); resolve(); });
    };
    invoke(cmd, { ...args, onEvent: channel }).catch((e) => {
      finish(); toast(String(e), "err");
    });
  });
}

// ---------------------------------------------------------------------------
// Carga de datos
// ---------------------------------------------------------------------------
async function load() {
  config = await invoke<AppConfig>("get_config");
  state = await invoke<AppState>("get_state");
  providers = await invoke<ProviderStatus>("provider_status");
  if (!allCategories.length) allCategories = await invoke<string[]>("categories");
}

// Auto-sincronización silenciosa (sin barra). NO bloquea la interfaz:
// usa su propio flag `syncing` (distinto de `busy`, que es para acciones manuales).
let autoTimer: ReturnType<typeof setInterval> | undefined;
let syncing = false;
async function quietSync() {
  if (busy || syncing || !config.canvas_token) return;
  syncing = true; refreshToolbar();
  const ch = new Channel<Progress>();
  const finish = (err?: string) => {
    syncing = false;
    if (err) toast(err, "err");
    // No re-renderizar si el usuario está en medio de una acción manual.
    if (!busy) load().then(() => { render(); applyTheme(); checkNotifications(); });
    else refreshToolbar();
  };
  ch.onmessage = (m) => {
    if (m.type === "done") finish();
    else if (m.type === "error") finish(m.message);
  };
  try {
    await invoke("sync", { lastSync: new Date().toISOString(), onEvent: ch });
  } catch (e) {
    finish(String(e));
  }
}
function startAutoSync() {
  if (autoTimer) clearInterval(autoTimer);
  autoTimer = setInterval(quietSync, 10 * 60 * 1000); // cada 10 min
}

// ---------------------------------------------------------------------------
// Tema dinámico desde el wallpaper (matugen / Material You)
// ---------------------------------------------------------------------------
function hexToRgba(hex: string, a: number): string {
  const h = hex.replace("#", "");
  const n = parseInt(h.length === 3 ? h.split("").map((c) => c + c).join("") : h, 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`;
}
async function applyTheme() {
  const root = document.documentElement;
  let pal: Record<string, string> | null = null;
  try { pal = await invoke<Record<string, string> | null>("get_theme"); } catch { pal = null; }
  if (!pal) { // tema Voltaic por defecto: limpiar overrides
    ["--ink","--panel","--raised","--raised-2","--line","--line-2","--paper","--paper-dim","--paper-faint",
     "--gold","--gold-bright","--gold-soft","--sage","--sage-soft","--rust","--rust-soft","--amber","--amber-soft"]
      .forEach((v) => root.style.removeProperty(v));
    return;
  }
  const g = (k: string, fb: string) => pal![k] || fb;
  const set = (v: string, val: string) => root.style.setProperty(v, val);
  const primary = g("primary", "#81d7b3");
  const secondary = g("secondary", "#e3badb");
  const tertiary = g("tertiary", "#d7bcf3");
  const error = g("error", "#ffb4ab");
  set("--ink", g("surface", "#15121a"));
  set("--panel", g("surface_container", "#211e26"));
  set("--raised", g("surface_container_high", "#2c2831"));
  set("--raised-2", g("surface_container_highest", "#37333c"));
  set("--line", g("outline_variant", "#4a4453"));
  set("--line-2", g("outline", "#958e9e"));
  set("--paper", g("on_surface", "#e7e0eb"));
  set("--paper-dim", g("on_surface_variant", "#ccc3d5"));
  set("--paper-faint", g("outline", "#958e9e"));
  set("--gold", primary); set("--gold-bright", g("primary_fixed_dim", primary)); set("--gold-soft", hexToRgba(primary, 0.16));
  set("--sage", secondary); set("--sage-soft", hexToRgba(secondary, 0.16));
  set("--amber", tertiary); set("--amber-soft", hexToRgba(tertiary, 0.17));
  set("--rust", error); set("--rust-soft", hexToRgba(error, 0.16));
}

// ---------------------------------------------------------------------------
// Notificaciones de plazos (1 semana, 3 días, 1 día, 6 h, 1 h antes)
// ---------------------------------------------------------------------------
const THRESHOLDS: { key: string; ms: number; label: string }[] = [
  { key: "1w", ms: 7 * 86400000, label: "1 semana" },
  { key: "3d", ms: 3 * 86400000, label: "3 días" },
  { key: "1d", ms: 1 * 86400000, label: "1 día" },
  { key: "6h", ms: 6 * 3600000, label: "6 horas" },
  { key: "1h", ms: 1 * 3600000, label: "1 hora" },
];
let notifTimer: ReturnType<typeof setInterval> | undefined;

async function checkNotifications() {
  const now = Date.now();
  const fired = new Set(state.notified);
  for (const a of pendingAssignments()) {
    const due = new Date(a.due_at!).getTime();
    if (isNaN(due) || due < now) continue;
    // Umbrales ya cruzados (now dentro de [due-th, due]).
    const crossed = THRESHOLDS.filter((t) => now >= due - t.ms);
    if (!crossed.length) continue;
    // El más urgente cruzado:
    const cur = crossed[crossed.length - 1];
    const curKey = `${a.id}@${cur.key}`;
    if (!fired.has(curKey)) {
      const fecha = new Date(a.due_at!).toLocaleString("es-CL", { day: "2-digit", month: "short", hour: "2-digit", minute: "2-digit" });
      try {
        await invoke("notify", { title: `⏳ ${cur.label} · ${a.name}`, body: `${a.course_name} — vence ${fecha}` });
      } catch { /* sin daemon de notificaciones */ }
    }
    // Marca como enviados todos los cruzados (evita avalancha tras reabrir).
    for (const t of crossed) {
      const k = `${a.id}@${t.key}`;
      if (!fired.has(k)) { fired.add(k); try { await invoke("mark_notified", { key: k }); } catch {} }
    }
  }
  // Eventos de calendario (detectados por IA / manuales).
  for (const ev of state.events) {
    if (!ev.date) continue;
    const due = new Date(ev.date.length === 10 ? ev.date + "T09:00:00" : ev.date).getTime();
    if (isNaN(due) || due < now) continue;
    const crossed = THRESHOLDS.filter((t) => now >= due - t.ms);
    if (!crossed.length) continue;
    const cur = crossed[crossed.length - 1];
    const curKey = `ev-${ev.id}@${cur.key}`;
    if (!fired.has(curKey)) {
      const fecha = new Date(due).toLocaleDateString("es-CL", { day: "2-digit", month: "short" });
      try { await invoke("notify", { title: `⏳ ${cur.label} · ${ev.title}`, body: `${ev.course_name || "Evento"} — ${fecha}` }); } catch {}
    }
    for (const t of crossed) {
      const k = `ev-${ev.id}@${t.key}`;
      if (!fired.has(k)) { fired.add(k); try { await invoke("mark_notified", { key: k }); } catch {} }
    }
  }
  state.notified = Array.from(fired);
}
function startNotifications() {
  if (notifTimer) clearInterval(notifTimer);
  notifTimer = setInterval(checkNotifications, 60 * 1000); // cada minuto
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------
function navItem(id: string, icon: string, label: string, count?: number) {
  const active = view === id ? "active" : "";
  const badge = count !== undefined && count > 0 ? `<span class="count">${count}</span>` : "";
  return `<div class="nav-item ${active}" data-nav="${id}"><span class="ic">${icon}</span><span>${label}</span>${badge}</div>`;
}

function renderShell() {
  const pend = pendingAssignments().length;
  const sinResumen = state.files.filter((f) => f.local_path && !f.summary && RESUMIBLES.includes(f.category)).length;
  app.innerHTML = `
    <aside class="sidebar">
      <div class="brand">
        <div class="logo">A</div>
        <div><h1>Aula</h1><span>Gestor UC</span></div>
      </div>
      ${navItem("dashboard", "◈", "Resumen")}
      ${navItem("calendario", "▦", "Calendario")}
      ${navItem("tareas", "✓", "Por hacer", pend)}
      ${navItem("ramos", "❖", "Ramos", state.courses.length)}
      ${navItem("archivos", "▤", "Archivos", state.files.length)}
      ${navItem("resumenes", "✦", "Resúmenes", sinResumen)}
      ${navItem("buscar", "⌕", "Buscar")}
      <div class="nav-sep"></div>
      ${navItem("ajustes", "⚙", "Ajustes")}
      <div class="sync-time">${state.last_sync ? "Sync: " + new Date(state.last_sync).toLocaleString("es-CL") : "Sin sincronizar"} · auto 10 min</div>
    </aside>
    <div class="main">
      <div class="topbar" id="topbar"></div>
      <div class="content" id="content"></div>
    </div>`;
  app.querySelectorAll("[data-nav]").forEach((n) =>
    n.addEventListener("click", () => { view = (n as HTMLElement).dataset.nav!; render(); })
  );
}

function refreshToolbar() {
  const tb = document.getElementById("topbar");
  if (!tb) return;
  const titles: Record<string, string> = {
    dashboard: "Resumen", calendario: "Calendario", tareas: "Por hacer", ramos: "Mis ramos",
    archivos: "Archivos", resumenes: "Resúmenes", buscar: "Buscar", ajustes: "Ajustes",
  };
  const title = view === "curso"
    ? (state.courses.find((c) => c.id === courseId)?.name ?? "Ramo")
    : (titles[view] ?? "");
  const sb = busy || syncing;
  const spin = sb ? '<span class="spin">↻</span>' : "↻";
  const actions = view === "ajustes" ? "" : `
    <button class="btn" data-act="sync" ${sb ? "disabled" : ""}>${spin} ${syncing ? "Sincronizando…" : "Sincronizar"}</button>
    <button class="btn" data-act="download_all" ${busy ? "disabled" : ""}>⬇ Descargar todo</button>
    <details class="dd">
      <summary class="btn ${busy ? "disabled" : ""}">🧠 Clasificar ▾</summary>
      <div class="dd-menu">
        <button class="dd-item" data-act="classify_all" ${busy ? "disabled" : ""}>Clasificar<small>Clasifica lo nuevo o sin clasificar + lee anuncios</small></button>
        <button class="dd-item" data-act="reclassify_all" ${busy ? "disabled" : ""}>Reclasificar todo<small>Rehace TODO con IA (respeta lo fijado a mano) y relee anuncios</small></button>
      </div>
    </details>
    <details class="dd">
      <summary class="btn primary ${busy ? "disabled" : ""}">✦ Resumir ▾</summary>
      <div class="dd-menu">
        <button class="dd-item" data-act="summarize_all" ${busy ? "disabled" : ""}>Resumir individual<small>Resume archivo por archivo (contexto limpio)</small></button>
        <button class="dd-item" data-act="summarize_everything" ${busy ? "disabled" : ""}>Resumir todo<small>Resumen de los contenidos del ramo: archivos + resúmenes + web</small></button>
      </div>
    </details>`;
  tb.innerHTML = `<h2>${esc(title)}</h2>${actions}`;
  tb.querySelectorAll("[data-act]").forEach((b) =>
    b.addEventListener("click", () => handleAction((b as HTMLElement).dataset.act!))
  );
}

async function handleAction(act: string) {
  if (busy) return;
  if (act === "sync") {
    if (!config.canvas_token) { toast("Configura el token de Canvas en Ajustes.", "err"); view = "ajustes"; render(); return; }
    await runProgress("sync", { lastSync: new Date().toISOString() });
  } else if (act === "download_all") {
    await runProgress("download_all");
  } else if (act === "classify_all") {
    await runProgress("classify_all", { force: false });
  } else if (act === "reclassify_all") {
    await runProgress("classify_all", { force: true });
  } else if (act === "summarize_all") {
    if (!providers.gemini && !providers.openrouter) { toast("No hay claves cloud en Nexo.", "err"); return; }
    await runProgress("summarize_all");
  } else if (act === "summarize_everything") {
    if (!providers.gemini && !providers.openrouter) { toast("No hay claves cloud en Nexo.", "err"); return; }
    await runProgress("summarize_everything");
  }
}

// ---------------------------------------------------------------------------
// Vistas
// ---------------------------------------------------------------------------
function bannerHtml(): string {
  const d = tokenDaysLeft();
  if (d === null || d > 15) return "";
  if (d <= 0) {
    return `<div class="banner crit"><span class="b-ic">⚠</span><div class="grow">
      Tu <b>token de Canvas caducó</b> (duran ${TOKEN_DAYS} días). Genera uno nuevo en Canvas → Cuenta → Configuración y actualízalo en Ajustes.</div>
      <button class="btn" data-nav-btn="ajustes">Ir a Ajustes</button></div>`;
  }
  return `<div class="banner"><span class="b-ic">⏳</span><div class="grow">
    Tu token de Canvas caduca en <b>${d} día${d === 1 ? "" : "s"}</b>. Conviene renovarlo pronto para no perder la sincronización.</div>
    <button class="btn" data-nav-btn="ajustes">Ir a Ajustes</button></div>`;
}

function render() {
  renderShell();
  refreshToolbar();
  const c = document.getElementById("content")!;
  if (view === "dashboard") c.innerHTML = viewDashboard();
  else if (view === "calendario") c.innerHTML = viewCalendario();
  else if (view === "tareas") c.innerHTML = viewTareas();
  else if (view === "ramos") c.innerHTML = viewRamos();
  else if (view === "curso") c.innerHTML = viewCurso();
  else if (view === "archivos") renderArchivos(c);
  else if (view === "resumenes") renderResumenes(c);
  else if (view === "buscar") renderBuscar(c);
  else if (view === "ajustes") renderAjustes(c);
  if (view !== "ajustes") {
    const b = bannerHtml();
    if (b) c.insertAdjacentHTML("afterbegin", b);
  }
  bindContent();
}

function viewDashboard(): string {
  const pend = pendingAssignments();
  const atrasadas = pend.filter((a) => dueInfo(a.due_at).cls === "red").length;
  const semana = pend.filter((a) => { const d = dueInfo(a.due_at); return d.cls === "yellow" || d.cls === "red"; }).length;
  const descargados = state.files.filter((f) => f.local_path).length;
  if (state.courses.length === 0) {
    return `<div class="empty"><div class="big">🎓</div><p>Aún no hay datos.</p><p>Configura tu token en <b>Ajustes</b> y pulsa <b>Sincronizar</b>.</p></div>`;
  }
  const próximas = pend.slice(0, 8).map((a) => {
    const d = dueInfo(a.due_at);
    return `<div class="row"><span class="dot ${d.cls || "blue"}"></span>
      <div class="grow"><div class="title">${esc(a.name)}</div><div class="sub">${esc(courseDisplay(a.course_id, a.course_name))}</div></div>
      <div class="right"><span class="pill ${d.cls}">${d.label}</span></div></div>`;
  }).join("") || `<div class="empty">Sin tareas pendientes 🎉</div>`;
  return `
    <div class="ledger">
      <div class="cell rust"><div class="n">${atrasadas}</div><div class="l">Atrasadas</div></div>
      <div class="cell amber"><div class="n">${semana}</div><div class="l">Esta semana</div></div>
      <div class="cell gold"><div class="n">${pend.length}</div><div class="l">Pendientes</div></div>
      <div class="cell sage"><div class="n">${descargados}<small>/${state.files.length}</small></div><div class="l">Descargados</div></div>
    </div>
    <div class="section-title">Próximas entregas</div>
    <div class="card">${próximas}</div>`;
}

let calYear = new Date().getFullYear();
let calMon = new Date().getMonth();
function viewCalendario(): string {
  const months = ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"];
  const dows = ["Lun", "Mar", "Mié", "Jue", "Vie", "Sáb", "Dom"];
  const past = concludedIds();
  const first = new Date(calYear, calMon, 1);
  const startDow = (first.getDay() + 6) % 7; // lunes = 0
  const daysInMonth = new Date(calYear, calMon + 1, 0).getDate();
  const today = new Date();
  const isThisMonth = today.getFullYear() === calYear && today.getMonth() === calMon;

  // Items del día: tareas (con plazo) + eventos (detectados por IA / manuales).
  const byDay: Record<number, { ts: number; html: string }[]> = {};
  const add = (day: number, ts: number, html: string) => { (byDay[day] ||= []).push({ ts, html }); };
  for (const a of state.assignments) {
    if (!a.due_at || a.user_status === "dismissed" || past.has(a.course_id)) continue;
    const d = new Date(a.due_at);
    if (d.getFullYear() === calYear && d.getMonth() === calMon) {
      const done = a.submitted || a.user_status === "done";
      const di = dueInfo(a.due_at);
      const cls = done ? "done" : di.cls === "red" ? "rust" : di.cls === "yellow" ? "amber" : "";
      add(d.getDate(), d.getTime(), `<div class="cal-ev ${cls}" data-open="${esc(a.html_url)}" title="${esc(a.name)} · ${esc(a.course_name)}">${esc(a.name)}</div>`);
    }
  }
  for (const ev of state.events) {
    if (!ev.date) continue;
    const d = new Date(ev.date.length === 10 ? ev.date + "T12:00:00" : ev.date);
    if (isNaN(d.getTime()) || d.getFullYear() !== calYear || d.getMonth() !== calMon) continue;
    const icon = ev.source === "manual" ? "📌" : "✦";
    add(d.getDate(), d.getTime(), `<div class="cal-ev evt" data-event="${esc(ev.id)}" title="${esc(ev.title)}${ev.course_name ? " · " + esc(ev.course_name) : ""}">${icon} ${esc(ev.title)}</div>`);
  }

  let cells = "";
  for (let i = 0; i < startDow; i++) cells += `<div class="cal-cell blank"></div>`;
  for (let day = 1; day <= daysInMonth; day++) {
    const items = (byDay[day] || []).sort((a, b) => a.ts - b.ts);
    const shown = items.slice(0, 4).map((x) => x.html).join("");
    const more = items.length > 4 ? `<div class="cal-more">+${items.length - 4} más</div>` : "";
    const todayCls = isThisMonth && today.getDate() === day ? "today" : "";
    cells += `<div class="cal-cell ${todayCls}"><span class="dnum">${day}</span>${shown}${more}</div>`;
  }
  const head = dows.map((d) => `<div class="cal-dow">${d}</div>`).join("");
  return `
    <div class="cal-bar">
      <div class="mtitle">${months[calMon]} ${calYear}</div>
      <div class="cal-nav">
        <button class="btn" data-cal="prev">‹</button>
        <button class="btn" data-cal="today">Hoy</button>
        <button class="btn" data-cal="next">›</button>
        <button class="btn primary" data-cal="add">➕ Evento</button>
      </div>
    </div>
    <div class="cal-grid">${head}${cells}</div>`;
}

function openAddEvent() {
  const ov = document.createElement("div");
  ov.className = "overlay";
  ov.innerHTML = `<div class="modal" style="width:min(440px,92vw)">
    <header><h3>Nuevo evento</h3><button class="btn" data-close>✕</button></header>
    <div class="body">
      <div class="field-row"><label>Título</label><input class="input" id="ev_title" placeholder="Ej: Estudiar para I2"></div>
      <div class="field-row"><label>Fecha</label><input class="input" id="ev_date" type="date"></div>
      <div class="field-row"><label>Ramo (opcional)</label><input class="input" id="ev_course" placeholder="Ej: Cálculo II"></div>
      <button class="btn primary" id="ev_save">Guardar evento</button>
    </div></div>`;
  ov.addEventListener("click", (e) => { if (e.target === ov || (e.target as HTMLElement).dataset.close !== undefined) ov.remove(); });
  ov.querySelector("#ev_save")!.addEventListener("click", async () => {
    const title = (ov.querySelector("#ev_title") as HTMLInputElement).value.trim();
    const date = (ov.querySelector("#ev_date") as HTMLInputElement).value;
    const course = (ov.querySelector("#ev_course") as HTMLInputElement).value.trim();
    if (!title || !date) { toast("Pon al menos título y fecha.", "err"); return; }
    try {
      await invoke("add_event", { title, date, courseName: course, id: `man-${Date.now()}` });
      ov.remove(); await load(); render(); toast("Evento añadido", "ok");
    } catch (e) { toast(String(e), "err"); }
  });
  document.body.appendChild(ov);
}

function openEventDetail(ev: CalEvent) {
  const ov = document.createElement("div");
  ov.className = "overlay";
  const origen = ev.source === "manual" ? "Añadido por ti" : "Detectado por IA en un documento";
  ov.innerHTML = `<div class="modal" style="width:min(440px,92vw)">
    <header><h3>${esc(ev.title)}</h3><button class="btn" data-close>✕</button></header>
    <div class="body">
      <p style="margin-bottom:8px"><b>Fecha:</b> ${esc(ev.date)}</p>
      ${ev.course_name ? `<p style="margin-bottom:8px"><b>Ramo:</b> ${esc(ev.course_name)}</p>` : ""}
      <p style="color:var(--paper-faint);margin-bottom:16px">${origen}</p>
      <button class="btn no" id="ev_del" style="border-color:var(--rust);color:var(--rust)">Eliminar evento</button>
    </div></div>`;
  ov.addEventListener("click", (e) => { if (e.target === ov || (e.target as HTMLElement).dataset.close !== undefined) ov.remove(); });
  ov.querySelector("#ev_del")!.addEventListener("click", async () => {
    try { await invoke("delete_event", { id: ev.id }); ov.remove(); await load(); render(); toast("Evento eliminado", "ok"); }
    catch (e) { toast(String(e), "err"); }
  });
  document.body.appendChild(ov);
}

function viewTareas(): string {
  const pend = pendingAssignments();
  const past = concludedIds();
  // "Hechas": entregadas en Canvas o marcadas a mano (excluyendo cursos pasados).
  const hechas = state.assignments.filter(
    (a) => !past.has(a.course_id) && a.user_status !== "dismissed" && (a.submitted || a.user_status === "done")
  );
  const descartadas = state.assignments.filter((a) => a.user_status === "dismissed" && !past.has(a.course_id));

  const actions = (a: Assignment) => `<div class="task-actions">
      <button class="mini ok" data-status="done:${a.id}" title="Marcar como hecha">✓</button>
      <button class="mini no" data-status="dismissed:${a.id}" title="Descartar">✕</button></div>`;

  const rowPend = (a: Assignment) => {
    const d = dueInfo(a.due_at);
    return `<div class="row">
      <span class="dot ${d.cls || "sage"}"></span>
      <div class="grow" data-open="${esc(a.html_url)}" style="cursor:pointer"><div class="title">${esc(a.name)}</div>
        <div class="sub">${esc(courseDisplay(a.course_id, a.course_name))}${a.points ? " · " + a.points + " pts" : ""}</div></div>
      <span class="pill ${d.cls}">${d.label}</span>${actions(a)}</div>`;
  };
  const rowDone = (a: Assignment) => `<div class="row" style="opacity:.62">
      <span class="dot sage"></span>
      <div class="grow" data-open="${esc(a.html_url)}" style="cursor:pointer"><div class="title">${esc(a.name)}</div><div class="sub">${esc(courseDisplay(a.course_id, a.course_name))}</div></div>
      <span class="pill sage">${a.user_status === "done" ? "Hecha (manual)" : a.state === "graded" ? "Calificada" : "Entregada"}</span>
      <div class="task-actions"><button class="mini" data-status="clear:${a.id}" title="Restaurar a pendiente">↺</button></div></div>`;
  const rowDisc = (a: Assignment) => `<div class="row" style="opacity:.55">
      <span class="dot"></span>
      <div class="grow"><div class="title">${esc(a.name)}</div><div class="sub">${esc(courseDisplay(a.course_id, a.course_name))}</div></div>
      <span class="pill">Descartada</span>
      <div class="task-actions"><button class="mini" data-status="clear:${a.id}" title="Restaurar">↺</button></div></div>`;

  const pendHtml = pend.length ? pend.map(rowPend).join("") : `<div class="empty">Sin tareas pendientes ✦</div>`;
  return `<div class="section-title">Pendientes <span class="tag">${pend.length}</span></div><div class="card">${pendHtml}</div>
    ${hechas.length ? `<div class="section-title">Hechas <span class="tag">${hechas.length}</span></div><div class="card">${hechas.slice(0, 40).map(rowDone).join("")}</div>` : ""}
    ${descartadas.length ? `<div class="section-title">Descartadas <span class="tag">${descartadas.length}</span></div><div class="card">${descartadas.map(rowDisc).join("")}</div>` : ""}`;
}

function viewRamos(): string {
  if (!state.courses.length) return `<div class="empty"><div class="big">Aula</div><p>Sincroniza para ver tus ramos.</p></div>`;
  const card = (co: Course) => {
    const nf = state.files.filter((f) => f.course_id === co.id).length;
    const na = state.assignments.filter((a) => a.course_id === co.id && !a.submitted && a.user_status !== "done" && a.user_status !== "dismissed").length;
    const tl = termLabel(co.term);
    const codeline = [esc(co.code || ""), tl ? esc(tl) : ""].filter(Boolean).join(" · ");
    return `<div class="course-card ${co.concluded ? "past" : ""}" data-course="${co.id}">
      <div class="code">${codeline || "&nbsp;"}</div>
      <div class="nm">${esc(co.name)}</div>
      <div class="meta"><span>${nf} archivos</span>${co.concluded ? "<span>· pasado</span>" : `<span>· ${na} pendientes</span>`}</div>
    </div>`;
  };
  const actuales = state.courses.filter((c) => !c.concluded);
  const pasados = state.courses.filter((c) => c.concluded);
  return `
    ${actuales.length ? `<div class="section-title">Ramos actuales <span class="tag">${actuales.length}</span></div><div class="grid">${actuales.map(card).join("")}</div>` : ""}
    ${pasados.length ? `<div class="section-title">Ramos pasados <span class="tag">${pasados.length}</span></div><div class="grid">${pasados.map(card).join("")}</div>` : ""}`;
}

// Orden de categorías en la vista de ramo (clases primero).
const CAT_ORDER = ["Clase", "Cápsula", "Ayudantía", "Guía/Ejercicios", "Laboratorio", "Tarea", "Prueba/Control", "Lectura/Paper", "Programa/Syllabus", "Administrativo", "Otro"];

// Vista de un ramo: lista de clases a la izquierda, resumen a la derecha.
function viewCurso(): string {
  const co = state.courses.find((c) => c.id === courseId);
  if (!co) return `<div class="empty">Ramo no encontrado.</div>`;
  const gens = state.course_summaries.filter((g) => g.course_id === courseId);
  const files = state.files.filter((f) => f.course_id === courseId && RESUMIBLES.includes(f.category));
  const nSum = files.filter((f) => f.summary).length;

  // Agrupar por categoría (orden definido), orden natural dentro del grupo (Clase 2 < Clase 10).
  const groups: { cat: string; items: CanvasFile[] }[] = [];
  const seen = new Set<string>();
  for (const cat of CAT_ORDER) {
    const items = files.filter((f) => f.category === cat)
      .sort((a, b) => a.filename.localeCompare(b.filename, "es", { numeric: true, sensitivity: "base" }));
    if (items.length) { groups.push({ cat, items }); seen.add(cat); }
  }
  // categorías no contempladas en CAT_ORDER, por si acaso
  for (const f of files) if (!seen.has(f.category)) {
    seen.add(f.category);
    const items = files.filter((x) => x.category === f.category).sort((a, b) => a.filename.localeCompare(b.filename, "es", { numeric: true }));
    groups.push({ cat: f.category, items });
  }

  // Selección por defecto: resumen general, o la primera clase con resumen, o la primera.
  const firstSum = files.find((f) => f.summary);
  if (!courseSel) courseSel = gens.length ? `gen:${gens[0].category}` : (firstSum ? `file:${firstSum.id}` : "");

  // Panel derecho.
  let bodyTitle = "", bodyMd = "", openPath = "", pdfPath = "", missingId = 0;
  if (courseSel.startsWith("gen:")) {
    const g = gens.find((x) => x.category === courseSel.slice(4));
    if (g) { bodyTitle = `${co.name} · Resumen general`; bodyMd = g.summary; }
  } else if (courseSel.startsWith("file:")) {
    const f = files.find((x) => x.id === Number(courseSel.slice(5)));
    if (f) { bodyTitle = f.filename; bodyMd = f.summary || ""; openPath = f.local_path || ""; pdfPath = f.summary_pdf || ""; if (!f.summary) missingId = f.id; }
  }

  const genItems = gens.map((g) => {
    const k = `gen:${g.category}`;
    return `<div class="cd-item gen ${courseSel === k ? "active" : ""}" data-cd="${esc(k)}">✦ Resumen general</div>`;
  }).join("");

  const groupsHtml = groups.map((gr) => {
    const rows = gr.items.map((f) => {
      const k = `file:${f.id}`;
      const has = !!f.summary;
      return `<div class="cd-item ${courseSel === k ? "active" : ""} ${has ? "" : "nosum"}" data-cd="${esc(k)}" title="${esc(f.filename)}">
        <span class="dot ${has ? "gold" : ""}"></span><span class="cd-nm">${esc(f.filename.replace(/\.[a-z0-9]+$/i, ""))}</span></div>`;
    }).join("");
    return `<div class="cd-group"><div class="cd-cat ${catColor(gr.cat)}">${esc(gr.cat)} <span>${gr.items.length}</span></div>${rows}</div>`;
  }).join("");

  let bodyHtml: string;
  if (bodyMd) {
    const pdfBtn = pdfPath ? `<button class="btn primary" data-openfile="${esc(pdfPath)}">📄 PDF del resumen</button>` : "";
    const fileBtn = openPath ? `<button class="btn ghost" data-openfile="${esc(openPath)}">Abrir original</button>` : "";
    bodyHtml = `<div class="cd-head"><h3>${esc(bodyTitle)}</h3><div class="cd-acts">${pdfBtn}${fileBtn}</div></div><div class="md">${renderMd(bodyMd)}</div>`;
  } else {
    bodyHtml = `<div class="empty"><div class="big">✦</div><p>${courseSel ? "Este archivo aún no tiene resumen." : "Selecciona una clase de la izquierda."}</p>${
      missingId ? `<button class="btn primary" data-dosum="${missingId}">✦ Resumir ahora</button>` : ""}</div>`;
  }

  return `
    <div class="cd-top">
      <button class="btn ghost" data-nav-btn="ramos">← Ramos</button>
      <div class="cd-title">${esc(co.name)} <span class="tag">${nSum} resúmenes</span></div>
    </div>
    <div class="course-detail">
      <div class="cd-list">${genItems}${groupsHtml || `<div class="empty">Sin clases resumibles.</div>`}</div>
      <div class="cd-body">${bodyHtml}</div>
    </div>`;
}

// Estado local de filtros de archivos.
let fCourse = 0; let fCat = ""; let fText = "";

function renderArchivos(c: HTMLElement) {
  const courseOpts = `<option value="0">Todos los ramos</option>` +
    state.courses.map((co) => `<option value="${co.id}" ${fCourse === co.id ? "selected" : ""}>${esc(co.name)}</option>`).join("");
  const catOpts = `<option value="">Todas las categorías</option>` +
    allCategories.map((k) => `<option value="${esc(k)}" ${fCat === k ? "selected" : ""}>${esc(k)}</option>`).join("");
  let files = state.files.slice();
  if (fCourse) files = files.filter((f) => f.course_id === fCourse);
  if (fCat) files = files.filter((f) => f.category === fCat);
  if (fText) files = files.filter((f) => f.filename.toLowerCase().includes(fText.toLowerCase()));
  files.sort((a, b) => a.course_name.localeCompare(b.course_name) || a.filename.localeCompare(b.filename));

  const past = concludedIds();
  const rows = files.map((f) => {
    // Selector de categoría (clasificación manual). Bloquea la auto-clasificación.
    const opts = `<option value="">— sin clasificar —</option>` +
      allCategories.map((k) => `<option value="${esc(k)}" ${f.category === k ? "selected" : ""}>${esc(k)}</option>`).join("");
    const catSel = `<select class="cat-sel ${catColor(f.category)}" data-fileid="${f.id}" title="Clasificar a mano">${opts}</select>`;
    const lock = f.category_manual ? `<span title="Categoría fijada a mano" style="color:var(--gold)">🔒</span>` : "";
    const dl = f.local_path ? "" : `<span class="pill amber">no descargado</span>`;
    const pst = past.has(f.course_id) ? `<span class="pill">pasado</span>` : "";
    const pdfBtn = f.summary_pdf ? `<button class="btn ghost" data-openfile="${esc(f.summary_pdf)}" title="Abrir PDF del resumen">📄 PDF</button>` : "";
    const sumBtn = f.summary
      ? `<button class="btn ghost" data-summary="${f.id}">Ver resumen</button>`
      : (f.local_path ? `<button class="btn ghost" data-dosum="${f.id}">✦ Resumir</button>` : "");
    return `<div class="row">
      <div class="grow"><div class="title" ${f.local_path ? `data-openfile="${esc(f.local_path)}" style="cursor:pointer"` : ""}>${esc(f.filename)}</div>
        <div class="sub">${esc(courseDisplay(f.course_id, f.course_name))}${f.folder ? " · " + esc(f.folder) : ""} ${f.size ? "· " + fmtSize(f.size) : ""}</div></div>
      ${pst} ${dl} ${lock} ${catSel} ${pdfBtn} ${sumBtn}</div>`;
  }).join("") || `<div class="empty">Sin archivos. Sincroniza y descarga.</div>`;

  c.innerHTML = `
    <div class="toolbar">
      <select class="input" id="fc">${courseOpts}</select>
      <select class="input" id="fk">${catOpts}</select>
      <input class="input search" id="ft" placeholder="Buscar archivo…" value="${esc(fText)}">
      <button class="btn" id="reclassAll" ${busy ? "disabled" : ""} title="Vuelve a clasificar TODO con IA (respeta los fijados a mano)">↻ Reclasificar todo</button>
    </div>
    <div class="card">${rows}</div>`;
}

function renderResumenes(c: HTMLElement) {
  const conResumen = state.files.filter((f) => f.summary);
  // Ramos con al menos un resumen individual → candidatos a resumen general.
  const coursesWithSum = state.courses.filter((co) => state.files.some((f) => f.course_id === co.id && f.summary));
  const genRows = coursesWithSum.map((co) => {
    const gens = state.course_summaries.filter((g) => g.course_id === co.id);
    const chips = gens.map((g) => `<span class="pill gold" data-coursesum="${g.course_id}@${esc(g.category)}" style="cursor:pointer">${esc(g.category)} ✦</span>`).join(" ");
    const sub = gens.length
      ? `General: ${chips}`
      : (co.concluded ? "Se generará al pulsar Resumir (ramo terminado)" : "Sin resumen general — púlsalo para compilar lo que llevas");
    return `<div class="row">
      <div class="grow"><div class="title">${esc(co.name)}${co.concluded ? ' · <span style="color:var(--paper-faint)">pasado</span>' : ""}</div>
        <div class="sub">${sub}</div></div>
      <button class="btn" data-gen="${co.id}">✦ ${co.concluded ? "Resumen general" : "Resumir hasta ahora"}</button></div>`;
  }).join("");

  const fileRows = conResumen.map((f) => `<div class="row" data-summary="${f.id}" style="cursor:pointer">
    <span class="dot gold"></span>
    <div class="grow"><div class="title">${esc(f.filename)}</div><div class="sub">${esc(courseDisplay(f.course_id, f.course_name))} · ${esc(f.category)}</div></div>
    <span class="pill gold">resumen ✦</span></div>`).join("");

  c.innerHTML = `
    ${coursesWithSum.length ? `<div class="section-title">Resumen general por ramo <span class="tag">hasta ahora</span></div><div class="card">${genRows}</div>` : ""}
    <div class="section-title">Resúmenes por clase <span class="tag">${conResumen.length}</span></div>
    ${conResumen.length ? `<div class="card">${fileRows}</div>` : `<div class="empty"><div class="big">✦</div><p>Aún no hay resúmenes.</p><p>Clasifica los archivos y pulsa <b>Resumir</b>.</p></div>`}`;
}

function renderBuscar(c: HTMLElement) {
  c.innerHTML = `
    <div class="toolbar">
      <input class="input search" id="sq" placeholder="Busca por significado: ‘derivadas parciales’, ‘rúbrica del proyecto’…">
      <button class="btn primary" id="sbtn">⌕ Buscar</button>
    </div>
    <div class="card" id="sres"><div class="empty">Búsqueda semántica sobre el contenido indexado (embeddings locales).</div></div>`;
  const run = async () => {
    const q = (document.getElementById("sq") as HTMLInputElement).value.trim();
    if (!q) return;
    const box = document.getElementById("sres")!;
    box.innerHTML = `<div class="empty"><span class="spin">↻</span> Buscando…</div>`;
    try {
      const hits = await invoke<SearchHit[]>("search", { query: q });
      box.innerHTML = hits.length ? hits.map((h) => `<div class="row" data-summary="${h.file_id}" style="cursor:pointer">
        <div class="grow"><div class="title">${esc(h.filename)}</div><div class="sub">${esc(h.course_name)} · ${esc(h.category)}</div></div>
        <span class="pill ${h.score > 0.6 ? "green" : h.score > 0.45 ? "yellow" : ""}">${(h.score * 100).toFixed(0)}%</span></div>`).join("")
        : `<div class="empty">Sin coincidencias. ¿Ya clasificaste los archivos (genera el índice)?</div>`;
      bindContent();
    } catch (e) { box.innerHTML = `<div class="empty">${esc(String(e))}</div>`; }
  };
  document.getElementById("sbtn")!.addEventListener("click", run);
  document.getElementById("sq")!.addEventListener("keydown", (e) => { if ((e as KeyboardEvent).key === "Enter") run(); });
}

function renderAjustes(c: HTMLElement) {
  const ps = (ok: boolean) => ok
    ? `<span class="dot green"></span><span style="color:var(--green)">configurada en Nexo</span>`
    : `<span class="dot red"></span><span style="color:var(--red)">no encontrada</span>`;
  c.innerHTML = `
    <div class="form">
      <div class="field">
        <label>URL de Canvas</label>
        <div class="hint">Instancia de tu universidad. Para la UC: https://cursos.canvas.uc.cl</div>
        <input class="input" id="c_url" value="${esc(config.canvas_url)}">
      </div>
      <div class="field">
        <label>Token de acceso de Canvas</label>
        <div class="hint">Canvas → Cuenta → Configuración → <b>Nuevo token de acceso</b>. Se guarda local con permisos 600.</div>
        <div class="kv"><input class="input" id="c_tok" type="password" value="${esc(config.canvas_token)}" placeholder="pega aquí tu token">
        <button class="btn" id="c_test">Probar conexión</button></div>
        ${(() => { const d = tokenDaysLeft(); if (d === null) return ""; const c = d <= 0 ? "var(--rust)" : d <= 15 ? "var(--amber)" : "var(--sage)"; return `<div class="status-line" style="margin-top:8px"><span class="dot" style="background:${c}"></span><span>${d <= 0 ? "Caducado — genera uno nuevo" : `Caduca en ${d} días`} (vida útil ${TOKEN_DAYS} días)</span></div>`; })()}
      </div>
      <div class="field">
        <label>Carpeta de descargas</label>
        <div class="hint">Donde se organizan los archivos por ramo y categoría.</div>
        <input class="input" id="c_dir" value="${esc(config.download_dir)}">
      </div>
      <div class="field">
        <label>IA local (Ollama)</label>
        <div class="hint">Clasificación e índice semántico, 100% local.</div>
        <div class="kv">
          <input class="input" id="c_oll" value="${esc(config.ollama_url)}" style="flex:1">
          <input class="input" id="c_cls" value="${esc(config.classify_model)}" title="modelo clasificación">
          <input class="input" id="c_emb" value="${esc(config.embed_model)}" title="modelo embeddings">
        </div>
      </div>
      <div class="field">
        <label>IA cloud para resúmenes (vía Nexo)</label>
        <div class="hint">Usa las claves guardadas en tu app Nexo (~/.config/cortado/config.json).</div>
        <div class="kv">
          <select class="input" id="c_prov" style="flex:1">
            <option value="gemini" ${config.summary_provider === "gemini" ? "selected" : ""}>Gemini (Google)</option>
            <option value="openrouter" ${config.summary_provider === "openrouter" ? "selected" : ""}>OpenRouter</option>
          </select>
          <input class="input" id="c_smodel" value="${esc(config.summary_model)}" title="modelo">
        </div>
        <div class="status-line">OpenRouter: ${ps(providers.openrouter)}</div>
        <div class="status-line">Gemini: ${ps(providers.gemini)}</div>
      </div>
      <div class="field">
        <label>Estética</label>
        <div class="hint">El tema dinámico sigue los colores de tu wallpaper (matugen / Quickshell). Desactívalo para usar el tema fijo "Voltaic".</div>
        <select class="input" id="c_dyn">
          <option value="true" ${config.dynamic_theme ? "selected" : ""}>Dinámico (wallpaper)</option>
          <option value="false" ${!config.dynamic_theme ? "selected" : ""}>Fijo (Voltaic)</option>
        </select>
      </div>
      <div><button class="btn primary" id="c_save">Guardar ajustes</button></div>
    </div>`;

  document.getElementById("c_test")!.addEventListener("click", async () => {
    await saveAjustes(false);
    try { const name = await invoke<string>("test_canvas"); toast(`✔ Conectado como ${name}`, "ok"); }
    catch (e) { toast(String(e), "err"); }
  });
  document.getElementById("c_save")!.addEventListener("click", () => saveAjustes(true));
}

async function saveAjustes(notify: boolean) {
  const g = (id: string) => (document.getElementById(id) as HTMLInputElement).value.trim();
  const newTok = g("c_tok");
  // Si el token cambió, reinicia la cuenta de los 120 días.
  const tokenCreated = newTok && newTok !== config.canvas_token
    ? new Date().toISOString()
    : config.token_created;
  config = {
    ...config,
    canvas_url: g("c_url"), canvas_token: newTok, download_dir: g("c_dir"),
    token_created: tokenCreated,
    ollama_url: g("c_oll"), classify_model: g("c_cls"), embed_model: g("c_emb"),
    summary_provider: g("c_prov"), summary_model: g("c_smodel"),
    dynamic_theme: g("c_dyn") === "true",
  };
  await invoke("save_config", { cfg: config });
  providers = await invoke<ProviderStatus>("provider_status");
  await applyTheme();
  if (notify) { toast("Ajustes guardados", "ok"); render(); }
}

// ---------------------------------------------------------------------------
// Modal de resumen
// ---------------------------------------------------------------------------
function openMarkdown(title: string, md: string, openPath?: string) {
  const ov = document.createElement("div");
  ov.className = "overlay";
  ov.innerHTML = `<div class="modal">
    <header><h3>${esc(title)}</h3>
      ${openPath ? `<button class="btn ghost" data-openfile="${esc(openPath)}">Abrir archivo</button>` : ""}
      <button class="btn" data-close>✕</button></header>
    <div class="body"><div class="md">${renderMd(md || "")}</div></div></div>`;
  ov.addEventListener("click", (e) => { if (e.target === ov || (e.target as HTMLElement).dataset.close !== undefined) ov.remove(); });
  ov.querySelector("[data-openfile]")?.addEventListener("click", () => openPath && invoke("open_path", { path: openPath }));
  document.body.appendChild(ov);
}
function showSummary(file: CanvasFile) {
  openMarkdown(file.filename, file.summary, file.local_path || undefined);
}

// ---------------------------------------------------------------------------
// Bindings delegados del contenido
// ---------------------------------------------------------------------------
async function setStatus(id: number, status: string) {
  try { await invoke("set_assignment_status", { assignmentId: id, status }); await load(); render(); }
  catch (e) { toast(String(e), "err"); }
}

function bindContent() {
  document.querySelectorAll("[data-open]").forEach((e) =>
    e.addEventListener("click", () => invoke("open_path", { path: (e as HTMLElement).dataset.open! })));
  document.querySelectorAll("[data-nav-btn]").forEach((e) =>
    e.addEventListener("click", () => { view = (e as HTMLElement).dataset.navBtn!; render(); }));
  document.querySelectorAll("[data-cal]").forEach((e) =>
    e.addEventListener("click", () => {
      const act = (e as HTMLElement).dataset.cal!;
      if (act === "add") { openAddEvent(); return; }
      if (act === "today") { calYear = new Date().getFullYear(); calMon = new Date().getMonth(); }
      else if (act === "prev") { calMon--; if (calMon < 0) { calMon = 11; calYear--; } }
      else if (act === "next") { calMon++; if (calMon > 11) { calMon = 0; calYear++; } }
      render();
    }));
  document.querySelectorAll("[data-event]").forEach((e) =>
    e.addEventListener("click", () => {
      const ev = state.events.find((x) => x.id === (e as HTMLElement).dataset.event);
      if (ev) openEventDetail(ev);
    }));
  document.querySelectorAll("[data-status]").forEach((e) =>
    e.addEventListener("click", (ev) => {
      ev.stopPropagation();
      const [kind, id] = (e as HTMLElement).dataset.status!.split(":");
      setStatus(Number(id), kind === "clear" ? "" : kind);
    }));
  document.querySelectorAll("[data-openfile]").forEach((e) =>
    e.addEventListener("click", (ev) => { ev.stopPropagation(); invoke("open_path", { path: (e as HTMLElement).dataset.openfile! }); }));
  document.querySelectorAll("[data-course]").forEach((e) =>
    e.addEventListener("click", () => { courseId = Number((e as HTMLElement).dataset.course); courseSel = ""; view = "curso"; render(); }));
  document.querySelectorAll("[data-cd]").forEach((e) =>
    e.addEventListener("click", () => { courseSel = (e as HTMLElement).dataset.cd!; render(); }));
  document.querySelectorAll("[data-summary]").forEach((e) =>
    e.addEventListener("click", () => {
      const f = state.files.find((x) => x.id === Number((e as HTMLElement).dataset.summary));
      if (f) showSummary(f);
    }));
  document.querySelectorAll("[data-coursesum]").forEach((e) =>
    e.addEventListener("click", () => {
      const [cid, cat] = (e as HTMLElement).dataset.coursesum!.split("@");
      const g = state.course_summaries.find((x) => x.course_id === Number(cid) && x.category === cat);
      if (g) openMarkdown(`${g.course_name} · ${g.category} (general)`, g.summary);
    }));
  document.querySelectorAll("[data-gen]").forEach((e) =>
    e.addEventListener("click", async () => {
      if (busy) return;
      if (!providers.gemini && !providers.openrouter) { toast("No hay claves cloud en Nexo.", "err"); return; }
      await runProgress("summarize_course", { courseId: Number((e as HTMLElement).dataset.gen) });
    }));
  document.querySelectorAll("[data-dosum]").forEach((e) =>
    e.addEventListener("click", async (ev) => {
      ev.stopPropagation();
      const id = Number((e as HTMLElement).dataset.dosum);
      (e as HTMLButtonElement).disabled = true; (e as HTMLElement).innerHTML = `<span class="spin">↻</span>`;
      try {
        await invoke<string>("summarize_file", { fileId: id });
        await load(); render();
        const f = state.files.find((x) => x.id === id); if (f) showSummary(f);
      } catch (err) { toast(String(err), "err"); render(); }
    }));
  // filtros de archivos
  // Clasificación manual por archivo.
  document.querySelectorAll(".cat-sel").forEach((e) =>
    e.addEventListener("change", async () => {
      const sel = e as HTMLSelectElement;
      try { await invoke("set_file_category", { fileId: Number(sel.dataset.fileid), category: sel.value }); await load(); renderArchivos(document.getElementById("content")!); bindContent(); }
      catch (err) { toast(String(err), "err"); }
    }));
  // Reclasificar TODO (force).
  const rc = document.getElementById("reclassAll");
  if (rc) rc.addEventListener("click", async () => {
    if (busy) return;
    if (!providers.gemini && !providers.openrouter) { toast("No hay claves cloud en Nexo.", "err"); return; }
    await runProgress("classify_all", { force: true });
  });
  const fc = document.getElementById("fc"); if (fc) fc.addEventListener("change", () => { fCourse = Number((fc as HTMLSelectElement).value); render(); });
  const fk = document.getElementById("fk"); if (fk) fk.addEventListener("change", () => { fCat = (fk as HTMLSelectElement).value; render(); });
  const ft = document.getElementById("ft"); if (ft) ft.addEventListener("input", () => { fText = (ft as HTMLInputElement).value; renderArchivos(document.getElementById("content")!); bindContent(); });
}

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------
(async function init() {
  await load();
  await applyTheme();
  if (!config.canvas_token) view = "ajustes";
  render();
  startAutoSync();
  startNotifications();
  checkNotifications();
  window.addEventListener("focus", () => { applyTheme(); });
  if (config.canvas_token) quietSync(); // sincroniza al iniciar
})();
