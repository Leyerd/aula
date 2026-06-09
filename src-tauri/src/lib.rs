mod ai;
mod canvas;
mod config;
mod store;
mod sys;

use config::AppConfig;
use serde::Serialize;
use std::path::PathBuf;
use store::AppState;
use tauri::ipc::Channel;

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Progress {
    Step { phase: String, current: u64, total: u64, message: String },
    Log { message: String },
    Done { message: String },
    Error { message: String },
}

#[derive(Serialize, Clone)]
struct ProviderStatus {
    openrouter: bool,
    gemini: bool,
}

#[derive(Serialize, Clone)]
struct SearchHit {
    file_id: u64,
    filename: String,
    course_name: String,
    category: String,
    score: f32,
}

// ----------------------------------------------------------------------------
// Helpers de rutas
// ----------------------------------------------------------------------------

fn sane(s: &str) -> String {
    s.chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Formatea el periodo de Canvas a "Primer/Segundo semestre 20XX" (o TAV).
fn term_label(term: &str) -> String {
    let t = term.trim();
    if t.is_empty() {
        return String::new();
    }
    let l = t.to_lowercase();
    let year: String = {
        let bytes = l.as_bytes();
        let mut found = String::new();
        let mut run = String::new();
        for &b in bytes {
            if b.is_ascii_digit() {
                run.push(b as char);
                if run.len() == 4 {
                    found = run.clone();
                    break;
                }
            } else {
                run.clear();
            }
        }
        found
    };
    let sem = if l.contains("primer") || l.contains("-1") || l.contains(" 1") || l.contains("/1") {
        "Primer semestre"
    } else if l.contains("segundo") || l.contains("-2") || l.contains(" 2") || l.contains("/2") {
        "Segundo semestre"
    } else if l.contains("tav") || l.contains("verano") {
        "TAV"
    } else {
        ""
    };
    match (sem.is_empty(), year.is_empty()) {
        (false, false) => format!("{} {}", sem, year),
        (false, true) => sem.to_string(),
        (true, false) => year,
        (true, true) => t.to_string(),
    }
}

/// Nombre de carpeta/etiqueta por ramo. Si el mismo nombre aparece en más de un
/// periodo (ramo repetido), añade el semestre para distinguirlos.
fn course_labels(state: &AppState) -> std::collections::HashMap<u64, String> {
    let mut counts: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    for c in &state.courses {
        *counts.entry(c.name.as_str()).or_insert(0) += 1;
    }
    let mut map = std::collections::HashMap::new();
    for c in &state.courses {
        let dup = counts.get(c.name.as_str()).copied().unwrap_or(0) > 1;
        let label = if dup {
            let suffix = {
                let tl = term_label(&c.term);
                if tl.is_empty() { format!("#{}", c.id) } else { tl }
            };
            format!("{} ({})", c.name, suffix)
        } else {
            c.name.clone()
        };
        map.insert(c.id, label);
    }
    map
}

/// Carpeta base de un ramo. Los cursos pasados van bajo "Cursos pasados/".
fn course_dir(cfg: &AppConfig, folder_name: &str, concluded: bool) -> PathBuf {
    let mut path = PathBuf::from(&cfg.download_dir);
    if concluded {
        path.push("Cursos pasados");
    }
    path.push(sane(folder_name));
    path
}

fn build_dest(cfg: &AppConfig, file: &store::CanvasFile, folder_name: &str, concluded: bool) -> PathBuf {
    let mut path = course_dir(cfg, folder_name, concluded);
    let folder = file
        .folder
        .trim_start_matches("course files")
        .trim_start_matches('/');
    for seg in folder.split('/') {
        if !seg.is_empty() {
            path.push(sane(seg));
        }
    }
    path.push(sane(&file.filename));
    path
}

/// Etiqueta de carpeta para una categoría (separa los resúmenes por tipo).
fn cat_folder(cat: &str) -> &str {
    match cat {
        "Ayudantía" => "Ayudantías resueltas",
        "Laboratorio" => "Laboratorios",
        "Guía/Ejercicios" => "Guías resueltas",
        "Prueba/Control" => "Pruebas y controles",
        "Clase" => "Clases",
        "Cápsula" => "Cápsulas",
        "Lectura/Paper" => "Lecturas",
        "Tarea" => "Tareas",
        _ => "Otros",
    }
}

/// Ruta del resumen en disco: <ramo>/_Resúmenes/<Tipo>/<archivo> — resumen.md
fn summary_dest(cfg: &AppConfig, file: &store::CanvasFile, folder_name: &str, concluded: bool) -> PathBuf {
    let mut path = course_dir(cfg, folder_name, concluded);
    path.push("_Resúmenes");
    path.push(sane(cat_folder(&file.category)));
    let stem = std::path::Path::new(&file.filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&file.filename);
    path.push(format!("{} — resumen.md", sane(stem)));
    path
}

/// Escribe el resumen en disco (best-effort).
fn write_summary_file(cfg: &AppConfig, file: &store::CanvasFile, folder_name: &str, concluded: bool, summary: &str) {
    let dest = summary_dest(cfg, file, folder_name, concluded);
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let header = format!(
        "# {}\n\n> Ramo: {} · Tipo: {}\n\n",
        file.filename, file.course_name, file.category
    );
    let _ = std::fs::write(&dest, format!("{}{}", header, summary));
}

/// Escribe el resumen GENERAL de un ramo (por categoría) en disco.
fn write_general_file(cfg: &AppConfig, folder_name: &str, concluded: bool, category: &str, course_name: &str, summary: &str) {
    let mut path = course_dir(cfg, folder_name, concluded);
    path.push("_Resúmenes");
    let cf = cat_folder(category);
    path.push(sane(cf));
    if let Err(_) = std::fs::create_dir_all(&path) {
        return;
    }
    path.push(format!("「Resumen general」 {}.md", sane(cf)));
    let header = format!("# Resumen general · {}\n\n> Ramo: {} · Sección: {}\n\n", cf, course_name, category);
    let _ = std::fs::write(&path, format!("{}{}", header, summary));
}

/// Genera (o regenera) los resúmenes GENERALES por categoría de los ramos dados,
/// a partir de los resúmenes individuales ya existentes.
async fn run_course_generals(cfg: &AppConfig, state: &mut AppState, course_ids: &[u64], use_web: bool, on_event: &Channel<Progress>) {
    let labels = course_labels(state);
    struct Job { cid: u64, name: String, folder: String, concluded: bool, cat: String, body: String }
    let mut jobs: Vec<Job> = Vec::new();
    for &cid in course_ids {
        let course = match state.courses.iter().find(|c| c.id == cid) {
            Some(c) => c.clone(),
            None => continue,
        };
        let folder = labels.get(&cid).cloned().unwrap_or_else(|| course.name.clone());
        let mut cats: Vec<String> = Vec::new();
        for f in &state.files {
            if f.course_id == cid && !f.summary.is_empty() && ai::is_summarizable(&f.category) && !cats.contains(&f.category) {
                cats.push(f.category.clone());
            }
        }
        for cat in cats {
            let mut body = String::new();
            for f in &state.files {
                if f.course_id == cid && f.category == cat && !f.summary.is_empty() {
                    body.push_str(&format!("## {}\n{}\n\n", f.filename, f.summary));
                }
            }
            if !body.trim().is_empty() {
                jobs.push(Job { cid, name: course.name.clone(), folder: folder.clone(), concluded: course.concluded, cat, body });
            }
        }
    }
    let total = jobs.len() as u64;
    for (i, job) in jobs.into_iter().enumerate() {
        let _ = on_event.send(Progress::Step {
            phase: "general".into(),
            current: i as u64 + 1,
            total,
            message: format!("{} · {}", job.name, job.cat),
        });
        // Resumen general ANTERIOR (para conservarlo e integrarlo).
        let previous = state.course_summaries.iter()
            .find(|c| c.course_id == job.cid && c.category == job.cat)
            .map(|c| c.summary.clone())
            .unwrap_or_default();
        // Búsqueda web complementaria (solo en "Resumir todo").
        let web = if use_web {
            let _ = on_event.send(Progress::Log { message: format!("🌐 Buscando en la web: {}…", job.name) });
            ai::web_search(&format!("{} {} conceptos clave universidad", job.name, job.cat), 6).await
        } else {
            String::new()
        };
        match ai::summarize_course(cfg, &job.name, &job.cat, &job.body, &previous, &web).await {
            Ok(s) => {
                if let Some(cs) = state.course_summaries.iter_mut().find(|c| c.course_id == job.cid && c.category == job.cat) {
                    cs.summary = s.clone();
                    cs.course_name = job.name.clone();
                } else {
                    state.course_summaries.push(store::CourseSummary {
                        course_id: job.cid,
                        course_name: job.name.clone(),
                        category: job.cat.clone(),
                        summary: s.clone(),
                    });
                }
                write_general_file(cfg, &job.folder, job.concluded, &job.cat, &job.name, &s);
            }
            Err(e) => {
                let _ = on_event.send(Progress::Log { message: format!("⚠ {} ({}): {}", job.name, job.cat, e) });
            }
        }
    }
}

/// Conjunto de course_id concluidos (cursos pasados → baja prioridad).
fn concluded_ids(state: &AppState) -> std::collections::HashSet<u64> {
    state.courses.iter().filter(|c| c.concluded).map(|c| c.id).collect()
}

/// Inserta/actualiza un evento de calendario DETECTADO por IA en un archivo.
fn upsert_detected_event(state: &mut AppState, file_idx: usize, date: &str, title_opt: &str) {
    let (fid, cat, fname, course) = {
        let f = &state.files[file_idx];
        (f.id, f.category.clone(), f.filename.clone(), f.course_name.clone())
    };
    let id = format!("det-{}", fid);
    let title = if title_opt.is_empty() {
        format!("{} · {}", cat, fname)
    } else {
        title_opt.to_string()
    };
    if let Some(ev) = state.events.iter_mut().find(|e| e.id == id) {
        ev.title = title;
        ev.date = date.to_string();
        ev.course_name = course;
    } else {
        state.events.push(store::CalEvent {
            id,
            title,
            date: date.to_string(),
            course_name: course,
            source: "detected".into(),
        });
    }
}

/// Inserta/actualiza un evento de calendario detectado en un ANUNCIO.
fn upsert_announcement_event(state: &mut AppState, ann_id: u64, n: usize, date: &str, title_opt: &str, course: &str, ann_title: &str) {
    let id = format!("ann-{}-{}", ann_id, n);
    let title = if title_opt.is_empty() { ann_title.to_string() } else { title_opt.to_string() };
    if let Some(ev) = state.events.iter_mut().find(|e| e.id == id) {
        ev.title = title;
        ev.date = date.to_string();
        ev.course_name = course.to_string();
    } else {
        state.events.push(store::CalEvent {
            id,
            title,
            date: date.to_string(),
            course_name: course.to_string(),
            source: "announcement".into(),
        });
    }
}

// ----------------------------------------------------------------------------
// Comandos básicos
// ----------------------------------------------------------------------------

#[tauri::command]
fn get_config() -> AppConfig {
    config::load()
}

#[tauri::command]
fn save_config(cfg: AppConfig) -> Result<(), String> {
    config::save(&cfg)
}

#[tauri::command]
fn provider_status() -> ProviderStatus {
    let (openrouter, gemini) = config::nexo_keys();
    ProviderStatus {
        openrouter: !openrouter.is_empty(),
        gemini: !gemini.is_empty(),
    }
}

#[tauri::command]
fn categories() -> Vec<String> {
    ai::CATEGORIES.iter().map(|s| s.to_string()).collect()
}

#[tauri::command]
async fn test_canvas() -> Result<String, String> {
    canvas::verify(&config::load()).await
}

#[tauri::command]
fn get_state() -> AppState {
    let mut state = store::load(&config::load().download_dir);
    // Incluir los eventos MANUALES del archivo compartido con Quickshell.
    state.events.retain(|e| e.source != "manual");
    state.events.extend(sys::read_user_events());
    state
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| e.to_string())
}

// ----------------------------------------------------------------------------
// Sincronización: ramos + tareas + listado de archivos
// ----------------------------------------------------------------------------

#[tauri::command]
async fn sync(last_sync: String, on_event: Channel<Progress>) -> Result<(), String> {
    let cfg = config::load();
    if cfg.canvas_token.trim().is_empty() {
        let _ = on_event.send(Progress::Error { message: "Falta el token de Canvas (Ajustes).".into() });
        return Ok(());
    }

    let _ = on_event.send(Progress::Log { message: "Conectando con Canvas…".into() });
    let courses = match canvas::fetch_courses(&cfg).await {
        Ok(c) => c,
        Err(e) => {
            let _ = on_event.send(Progress::Error { message: e });
            return Ok(());
        }
    };
    let _ = on_event.send(Progress::Log { message: format!("{} ramos encontrados.", courses.len()) });

    let mut assignments = Vec::new();
    let mut files = Vec::new();
    let mut announcements = Vec::new();
    let total = courses.len() as u64;
    for (i, course) in courses.iter().enumerate() {
        let _ = on_event.send(Progress::Step {
            phase: "sync".into(),
            current: i as u64 + 1,
            total,
            message: course.name.clone(),
        });
        if let Ok(mut a) = canvas::fetch_assignments(&cfg, course).await {
            assignments.append(&mut a);
        }
        if let Ok(mut list) = canvas::fetch_files(&cfg, course).await {
            files.append(&mut list);
        }
        let mut anns = canvas::fetch_announcements(&cfg, course).await;
        announcements.append(&mut anns);
    }

    // Fusión contra el estado MÁS RECIENTE en disco (no el del inicio): mientras
    // sincronizábamos —que tarda— el usuario pudo clasificar/descargar/resumir.
    // Así no pisamos categorías, descargas, resúmenes ni overrides hechos en paralelo.
    let current = store::load(&cfg.download_dir);
    let cur_status: std::collections::HashMap<u64, String> =
        current.assignments.iter().map(|a| (a.id, a.user_status.clone())).collect();
    let mut cur_files: std::collections::HashMap<u64, store::CanvasFile> =
        current.files.into_iter().map(|f| (f.id, f)).collect();
    for a in assignments.iter_mut() {
        if let Some(st) = cur_status.get(&a.id) {
            a.user_status = st.clone();
        }
    }
    for f in files.iter_mut() {
        if let Some(old) = cur_files.remove(&f.id) {
            f.local_path = old.local_path;
            f.category = old.category;
            f.category_manual = old.category_manual;
            f.content_done = old.content_done;
            f.summary = old.summary;
            f.embedding = old.embedding;
            f.summary_pdf = old.summary_pdf;
        }
    }
    // Preservar qué anuncios ya leyó la IA (para no reprocesarlos al clasificar).
    let cur_ann: std::collections::HashMap<u64, bool> =
        current.announcements.iter().map(|a| (a.id, a.dates_done)).collect();
    for an in announcements.iter_mut() {
        if let Some(done) = cur_ann.get(&an.id) {
            an.dates_done = *done;
        }
    }

    let state = AppState {
        courses,
        assignments,
        files,
        course_summaries: current.course_summaries,
        events: current.events,
        announcements,
        last_sync,
        notified: current.notified,
    };
    store::save(&cfg.download_dir, &state)?;
    // Integración con el entorno: calendario .ics + To Do de Quickshell (best-effort).
    let _ = sys::export_ics(&state);
    let _ = sys::sync_quickshell_todo(&state);
    let _ = on_event.send(Progress::Done {
        message: format!(
            "Sincronizado: {} ramos, {} tareas, {} archivos.",
            state.courses.len(),
            state.assignments.len(),
            state.files.len()
        ),
    });
    Ok(())
}

// ----------------------------------------------------------------------------
// Descarga de todos los archivos
// ----------------------------------------------------------------------------

#[tauri::command]
async fn download_all(on_event: Channel<Progress>) -> Result<(), String> {
    let cfg = config::load();
    let mut state = store::load(&cfg.download_dir);
    let past = concluded_ids(&state);
    let labels = course_labels(&state);

    // Reconciliar con el DISCO: detectar archivos ya descargados (aunque el estado
    // no lo registre) y olvidar rutas cuyo archivo ya no existe. Así no se re-descarga.
    let mut reconciled = 0u64;
    for i in 0..state.files.len() {
        let concluded = past.contains(&state.files[i].course_id);
        let folder = labels
            .get(&state.files[i].course_id)
            .cloned()
            .unwrap_or_else(|| state.files[i].course_name.clone());
        let dest = build_dest(&cfg, &state.files[i], &folder, concluded);
        if dest.exists() {
            let dp = dest.to_string_lossy().to_string();
            if state.files[i].local_path != dp {
                state.files[i].local_path = dp;
                reconciled += 1;
            }
        } else if !state.files[i].local_path.is_empty()
            && !std::path::Path::new(&state.files[i].local_path).exists()
        {
            state.files[i].local_path.clear(); // estaba marcado pero ya no está
        }
    }
    if reconciled > 0 {
        let _ = store::save(&cfg.download_dir, &state);
        let _ = on_event.send(Progress::Log {
            message: format!("{} archivos ya estaban en disco (no se re-descargan).", reconciled),
        });
    }

    let mut pending: Vec<usize> = state
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.local_path.is_empty())
        .map(|(i, _)| i)
        .collect();
    // Cursos pasados al final (menor prioridad).
    pending.sort_by_key(|&i| past.contains(&state.files[i].course_id));
    let total = pending.len() as u64;
    if total == 0 {
        let _ = on_event.send(Progress::Done { message: "Todo descargado (nada nuevo).".into() });
        return Ok(());
    }
    for (n, &idx) in pending.iter().enumerate() {
        let file = state.files[idx].clone();
        let concluded = past.contains(&file.course_id);
        let folder = labels.get(&file.course_id).cloned().unwrap_or_else(|| file.course_name.clone());
        let dest = build_dest(&cfg, &file, &folder, concluded);
        let _ = on_event.send(Progress::Step {
            phase: "download".into(),
            current: n as u64 + 1,
            total,
            message: format!("{} · {}", file.course_name, file.filename),
        });
        match canvas::download_file(&cfg, &file, &dest).await {
            Ok(_) => state.files[idx].local_path = dest.to_string_lossy().to_string(),
            Err(e) => {
                let _ = on_event.send(Progress::Log { message: format!("⚠ {}: {}", file.filename, e) });
            }
        }
        // Guardado incremental para no perder progreso.
        if n % 5 == 0 {
            let _ = store::save(&cfg.download_dir, &state);
        }
    }
    store::save(&cfg.download_dir, &state)?;
    let _ = on_event.send(Progress::Done { message: format!("{} archivos descargados.", total) });
    Ok(())
}

// ----------------------------------------------------------------------------
// Clasificación + embeddings (IA local)
// ----------------------------------------------------------------------------

#[tauri::command]
async fn classify_all(force: bool, on_event: Channel<Progress>) -> Result<(), String> {
    let cfg = config::load();
    let mut state = store::load(&cfg.download_dir);
    let past = concluded_ids(&state);
    // Pendientes: sin categoría, o descargados que aún NO se clasificaron por contenido
    // (p. ej. se clasificaron por nombre y luego se descargaron → ahora por contenido).
    let mut pending: Vec<usize> = state
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.category_manual
            && (force || f.category.is_empty() || (!f.local_path.is_empty() && !f.content_done)))
        .map(|(i, _)| i)
        .collect();
    pending.sort_by_key(|&i| past.contains(&state.files[i].course_id));
    let total = pending.len() as u64;

    // Si se fuerza, reprocesar también todos los anuncios (releer fechas).
    if force {
        for a in state.announcements.iter_mut() {
            a.dates_done = false;
        }
    }
    let ann_pending = state.announcements.iter().any(|a| !a.dates_done);
    if total == 0 && !ann_pending {
        let _ = on_event.send(Progress::Done { message: "Todo clasificado.".into() });
        return Ok(());
    }

    // Procesamiento MODULAR: ventana pequeña → leer, clasificar, detectar fechas,
    // GUARDAR y avisar; repetir. Así la app se actualiza constantemente, sin leerlo todo primero.
    let mut done = 0u64;
    let window = 12usize;
    let mut start = 0usize;
    while start < pending.len() {
        let end = (start + window).min(pending.len());
        let slice: Vec<usize> = pending[start..end].to_vec();

        let mut content: Vec<(usize, String)> = Vec::new();
        let mut name_only: Vec<usize> = Vec::new();
        for &idx in &slice {
            // (0) DETERMINISTA por carpeta de Canvas (lo más fiable). Si la carpeta
            //     indica el tipo, se asigna directo sin IA → consistente y rápido.
            if let Some(cat) = ai::folder_category(&state.files[idx].folder) {
                state.files[idx].category = cat.to_string();
                state.files[idx].content_done = true;
                continue;
            }
            if state.files[idx].local_path.is_empty() {
                name_only.push(idx);
                continue;
            }
            let text = ai::extract_text(&state.files[idx].local_path, 4000);
            if text.trim().is_empty() {
                state.files[idx].content_done = true; // descargado pero sin texto
                name_only.push(idx);
            } else {
                content.push((idx, text));
            }
        }

        // Por CONTENIDO (+ detección de fechas importantes → eventos de calendario).
        if !content.is_empty() {
            let items: Vec<(String, String, String, String)> = content
                .iter()
                .map(|(i, text)| {
                    let f = &state.files[*i];
                    (f.filename.clone(), f.folder.clone(), f.course_name.clone(), text.clone())
                })
                .collect();
            let res = ai::classify_batch_content(&cfg, &items).await;
            for (k, (i, text)) in content.iter().enumerate() {
                if let Some(r) = res.get(k) {
                    state.files[*i].category = r.category.clone();
                    if !r.date.is_empty() {
                        upsert_detected_event(&mut state, *i, &r.date, &r.event);
                    }
                }
                state.files[*i].content_done = true;
                if state.files[*i].embedding.is_empty() {
                    if let Ok(v) = ai::embed(&cfg, &format!("{}\n{}", state.files[*i].filename, text)).await {
                        state.files[*i].embedding = v;
                    }
                }
            }
        }

        // Solo NOMBRE (no descargado o no extraíble): provisional.
        if !name_only.is_empty() {
            let items: Vec<(String, String, String)> = name_only
                .iter()
                .map(|&i| {
                    let f = &state.files[i];
                    (f.filename.clone(), f.folder.clone(), f.course_name.clone())
                })
                .collect();
            let cats = ai::classify_batch(&cfg, &items).await;
            for (k, &i) in name_only.iter().enumerate() {
                if let Some(c) = cats.get(k) {
                    state.files[i].category = c.clone();
                }
            }
        }

        done += slice.len() as u64;
        let _ = store::save(&cfg.download_dir, &state);
        let _ = on_event.send(Progress::Step { phase: "classify".into(), current: done, total, message: format!("Clasificando… ({}/{})", done, total) });
        start = end;
    }

    // ANUNCIOS: la IA los lee para detectar fechas importantes (pruebas, entregas,
    // ayudantías…) que muchas veces SOLO se avisan ahí → al calendario.
    let ann_idx: Vec<usize> = state
        .announcements
        .iter()
        .enumerate()
        .filter(|(_, a)| !a.dates_done && !a.message.trim().is_empty())
        .map(|(i, _)| i)
        .collect();
    let ann_total = ann_idx.len();
    if ann_total > 0 {
        let _ = on_event.send(Progress::Log { message: format!("📢 Leyendo {} anuncios para detectar fechas…", ann_total) });
        let mut astart = 0usize;
        let awin = 8usize;
        while astart < ann_idx.len() {
            let aend = (astart + awin).min(ann_idx.len());
            let slice: Vec<usize> = ann_idx[astart..aend].to_vec();
            let items: Vec<(String, String, String)> = slice
                .iter()
                .map(|&i| {
                    let a = &state.announcements[i];
                    (a.title.clone(), a.course_name.clone(), a.message.clone())
                })
                .collect();
            let res = ai::detect_dates(&cfg, &items).await;
            for (k, &i) in slice.iter().enumerate() {
                let (ann_id, course, ann_title) = {
                    let a = &state.announcements[i];
                    (a.id, a.course_name.clone(), a.title.clone())
                };
                if let Some(fechas) = res.get(k) {
                    for (n, (fecha, evento)) in fechas.iter().enumerate() {
                        upsert_announcement_event(&mut state, ann_id, n, fecha, evento, &course, &ann_title);
                    }
                }
                state.announcements[i].dates_done = true;
            }
            let _ = store::save(&cfg.download_dir, &state);
            let _ = on_event.send(Progress::Step { phase: "classify".into(), current: aend as u64, total: ann_total as u64, message: format!("Anuncios… ({}/{})", aend, ann_total) });
            astart = aend;
        }
    }

    // Mantener el calendario/.ics al día con las fechas detectadas.
    let _ = sys::export_ics(&state);
    store::save(&cfg.download_dir, &state)?;
    let _ = on_event.send(Progress::Done { message: format!("{} archivos clasificados · {} anuncios leídos.", total, ann_total) });
    Ok(())
}

// ----------------------------------------------------------------------------
// Resúmenes (IA cloud)
// ----------------------------------------------------------------------------

#[tauri::command]
async fn summarize_file(file_id: u64) -> Result<String, String> {
    let cfg = config::load();
    let mut state = store::load(&cfg.download_dir);
    let idx = state
        .files
        .iter()
        .position(|f| f.id == file_id)
        .ok_or("Archivo no encontrado")?;
    let file = state.files[idx].clone();
    if file.local_path.is_empty() {
        return Err("El archivo no está descargado.".into());
    }
    let concluded = concluded_ids(&state).contains(&file.course_id);
    let folder = course_labels(&state).get(&file.course_id).cloned().unwrap_or_else(|| file.course_name.clone());
    let text = ai::extract_text(&file.local_path, 30000);
    let summary = ai::summarize(&cfg, &file.filename, &file.category, &text).await?;
    state.files[idx].summary = summary.clone();
    write_summary_file(&cfg, &file, &folder, concluded, &summary);
    store::save(&cfg.download_dir, &state)?;
    Ok(summary)
}

/// Genera los resúmenes INDIVIDUALES (archivo por archivo) de todo lo pendiente.
/// Cada archivo es una llamada cloud independiente (contexto limpio, sin mezclar).
/// Devuelve cuántos archivos se procesaron.
async fn run_individual_summaries(cfg: &AppConfig, state: &mut AppState, on_event: &Channel<Progress>) -> u64 {
    let past = concluded_ids(state);
    let labels = course_labels(state);
    let mut pending: Vec<usize> = state
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            !f.local_path.is_empty()
                && f.summary.is_empty()
                && ai::is_summarizable(&f.category)
        })
        .map(|(i, _)| i)
        .collect();
    // Cursos pasados al final (menor prioridad).
    pending.sort_by_key(|&i| past.contains(&state.files[i].course_id));
    let total = pending.len() as u64;
    for (n, &idx) in pending.iter().enumerate() {
        let file = state.files[idx].clone();
        let concluded = past.contains(&file.course_id);
        let _ = on_event.send(Progress::Step {
            phase: "summarize".into(),
            current: n as u64 + 1,
            total,
            message: file.filename.clone(),
        });
        let folder = labels.get(&file.course_id).cloned().unwrap_or_else(|| file.course_name.clone());
        let text = ai::extract_text(&file.local_path, 30000);
        match ai::summarize(cfg, &file.filename, &file.category, &text).await {
            Ok(s) => { state.files[idx].summary = s.clone(); write_summary_file(cfg, &file, &folder, concluded, &s); }
            Err(e) => {
                let _ = on_event.send(Progress::Log { message: format!("⚠ {}: {}", file.filename, e) });
            }
        }
        let _ = store::save(&cfg.download_dir, state);
    }
    total
}

/// "Resumir individual": resume archivo por archivo (resúmenes modulares).
#[tauri::command]
async fn summarize_all(on_event: Channel<Progress>) -> Result<(), String> {
    let cfg = config::load();
    let mut state = store::load(&cfg.download_dir);
    let total = run_individual_summaries(&cfg, &mut state, &on_event).await;
    store::save(&cfg.download_dir, &state)?;
    let msg = if total == 0 {
        "Nada que resumir (clasifica primero).".to_string()
    } else {
        format!("{} resúmenes individuales generados.", total)
    };
    let _ = on_event.send(Progress::Done { message: msg });
    Ok(())
}

/// "Resumir todo": (1) asegura los resúmenes individuales y (2) genera el
/// RESUMEN GENERAL de los contenidos de CADA ramo, basado en todos los archivos
/// + sus resúmenes + BÚSQUEDA WEB para complementar. Incremental: conserva el
/// resumen general anterior y lo integra con lo nuevo.
#[tauri::command]
async fn summarize_everything(on_event: Channel<Progress>) -> Result<(), String> {
    let cfg = config::load();
    let mut state = store::load(&cfg.download_dir);
    // 1) Resúmenes individuales de lo que falte.
    let n = run_individual_summaries(&cfg, &mut state, &on_event).await;
    store::save(&cfg.download_dir, &state)?;
    // 2) Resumen general (con web) de TODOS los ramos que ya tengan resúmenes.
    let with_sum: Vec<u64> = state
        .courses
        .iter()
        .map(|c| c.id)
        .filter(|cid| state.files.iter().any(|f| f.course_id == *cid && !f.summary.is_empty()))
        .collect();
    if with_sum.is_empty() {
        let _ = on_event.send(Progress::Done { message: "Nada que resumir (clasifica y descarga primero).".into() });
        return Ok(());
    }
    run_course_generals(&cfg, &mut state, &with_sum, true, &on_event).await;
    store::save(&cfg.download_dir, &state)?;
    let _ = on_event.send(Progress::Done {
        message: format!("Listo: {} resúmenes individuales + resumen general de {} ramos.", n, with_sum.len()),
    });
    Ok(())
}

/// Genera el RESUMEN GENERAL de un ramo (todas sus secciones) bajo demanda,
/// con búsqueda web e integrando el resumen anterior. Sirve para ramos en curso.
#[tauri::command]
async fn summarize_course(course_id: u64, on_event: Channel<Progress>) -> Result<(), String> {
    let cfg = config::load();
    let mut state = store::load(&cfg.download_dir);
    let exists = state.courses.iter().any(|c| c.id == course_id);
    if !exists {
        let _ = on_event.send(Progress::Error { message: "Ramo no encontrado".into() });
        return Ok(());
    }
    let has_sum = state.files.iter().any(|f| f.course_id == course_id && !f.summary.is_empty());
    if !has_sum {
        let _ = on_event.send(Progress::Done {
            message: "Primero resume las clases del ramo (Resumir → individual).".into(),
        });
        return Ok(());
    }
    run_course_generals(&cfg, &mut state, &[course_id], true, &on_event).await;
    store::save(&cfg.download_dir, &state)?;
    let _ = on_event.send(Progress::Done { message: "Resumen general listo.".into() });
    Ok(())
}

// ----------------------------------------------------------------------------
// Búsqueda semántica
// ----------------------------------------------------------------------------

#[tauri::command]
async fn search(query: String) -> Result<Vec<SearchHit>, String> {
    let cfg = config::load();
    let state = store::load(&cfg.download_dir);
    let qv = ai::embed(&cfg, &query).await?;
    let mut hits: Vec<SearchHit> = state
        .files
        .iter()
        .filter(|f| !f.embedding.is_empty())
        .map(|f| SearchHit {
            file_id: f.id,
            filename: f.filename.clone(),
            course_name: f.course_name.clone(),
            category: f.category.clone(),
            score: ai::cosine(&qv, &f.embedding),
        })
        .collect();
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(15);
    Ok(hits)
}

/// Lanza una notificación de escritorio.
#[tauri::command]
fn notify(title: String, body: String) -> Result<(), String> {
    sys::notify(&title, &body)
}

/// Paleta Material You del wallpaper (matugen), o null si no existe.
#[tauri::command]
fn get_theme() -> Option<serde_json::Value> {
    let cfg = config::load();
    if !cfg.dynamic_theme {
        return None;
    }
    sys::matugen_palette()
}

/// Marca un aviso como ya enviado (clave "<id>@<umbral>").
#[tauri::command]
fn mark_notified(key: String) -> Result<(), String> {
    let cfg = config::load();
    let mut state = store::load(&cfg.download_dir);
    if !state.notified.contains(&key) {
        state.notified.push(key);
        // Evita crecer sin límite.
        if state.notified.len() > 2000 {
            let extra = state.notified.len() - 2000;
            state.notified.drain(0..extra);
        }
        store::save(&cfg.download_dir, &state)?;
    }
    Ok(())
}

/// Añade un evento manual (archivo compartido con Quickshell) y reexporta el .ics.
#[tauri::command]
fn add_event(title: String, date: String, course_name: String, id: String) -> Result<(), String> {
    let mut evs = sys::read_user_events();
    let eid = if id.trim().is_empty() { format!("man-{}", evs.len() + 1) } else { id };
    if let Some(ev) = evs.iter_mut().find(|e| e.id == eid) {
        ev.title = title;
        ev.date = date;
        ev.course_name = course_name;
    } else {
        evs.push(store::CalEvent { id: eid, title, date, course_name, source: "manual".into() });
    }
    sys::write_user_events(&evs)?;
    let cfg = config::load();
    let _ = sys::export_ics(&store::load(&cfg.download_dir));
    Ok(())
}

/// Borra un evento por id (manual del archivo compartido, o detectado del estado).
#[tauri::command]
fn delete_event(id: String) -> Result<(), String> {
    let mut evs = sys::read_user_events();
    let before = evs.len();
    evs.retain(|e| e.id != id);
    if evs.len() != before {
        sys::write_user_events(&evs)?;
    }
    let cfg = config::load();
    let mut state = store::load(&cfg.download_dir);
    let b2 = state.events.len();
    state.events.retain(|e| e.id != id);
    if state.events.len() != b2 {
        store::save(&cfg.download_dir, &state)?;
    }
    let _ = sys::export_ics(&state);
    Ok(())
}

/// Reexporta calendario .ics + To Do de Quickshell desde el estado actual.
#[tauri::command]
fn export_integrations() -> Result<(), String> {
    let cfg = config::load();
    let state = store::load(&cfg.download_dir);
    let _ = sys::export_ics(&state);
    sys::sync_quickshell_todo(&state)
}

/// Clasificación MANUAL de un archivo: fija la categoría y la "bloquea"
/// (no se vuelve a auto-clasificar). Categoría vacía → desbloquea.
#[tauri::command]
fn set_file_category(file_id: u64, category: String) -> Result<(), String> {
    let cfg = config::load();
    let mut state = store::load(&cfg.download_dir);
    let idx = state
        .files
        .iter()
        .position(|f| f.id == file_id)
        .ok_or("Archivo no encontrado")?;
    if category.trim().is_empty() {
        state.files[idx].category_manual = false; // desbloquear (volverá a auto-clasificarse)
    } else {
        state.files[idx].category = category;
        state.files[idx].category_manual = true;
        state.files[idx].content_done = true;
    }
    store::save(&cfg.download_dir, &state)
}

/// Override manual de una tarea: "done" (hecha), "dismissed" (descartada) o "" (restaurar).
#[tauri::command]
fn set_assignment_status(assignment_id: u64, status: String) -> Result<(), String> {
    let cfg = config::load();
    let mut state = store::load(&cfg.download_dir);
    let idx = state
        .assignments
        .iter()
        .position(|a| a.id == assignment_id)
        .ok_or("Tarea no encontrada")?;
    state.assignments[idx].user_status = match status.as_str() {
        "done" | "dismissed" => status,
        _ => String::new(),
    };
    store::save(&cfg.download_dir, &state)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            provider_status,
            categories,
            test_canvas,
            get_state,
            open_path,
            sync,
            download_all,
            classify_all,
            summarize_file,
            summarize_all,
            summarize_everything,
            search,
            set_assignment_status,
            set_file_category,
            summarize_course,
            notify,
            get_theme,
            mark_notified,
            add_event,
            delete_event,
            export_integrations
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
