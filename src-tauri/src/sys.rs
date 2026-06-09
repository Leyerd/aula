use crate::store::{AppState, Course};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Convierte una fecha-hora ISO (UTC, de Canvas) a la fecha LOCAL "YYYY-MM-DD".
/// Evita el desfase de 1 día con entregas nocturnas. Fallback: los 10 primeros chars.
pub fn local_date(iso: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(iso) {
        Ok(dt) => dt
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string(),
        Err(_) => iso.chars().take(10).collect(),
    }
}

/// Archivo de eventos MANUALES compartido con Quickshell (lo escriben ambos).
pub fn user_events_path() -> PathBuf {
    home().join(".local/state/quickshell/user/aula_user_events.json")
}

/// Lee los eventos manuales del archivo compartido.
pub fn read_user_events() -> Vec<crate::store::CalEvent> {
    match std::fs::read_to_string(user_events_path()) {
        Ok(s) => serde_json::from_str::<Vec<Value>>(&s)
            .map(|arr| {
                arr.iter()
                    .map(|v| crate::store::CalEvent {
                        id: v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        title: v.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        date: v.get("date").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        course_name: v.get("course").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        source: "manual".into(),
                    })
                    .filter(|e| !e.title.is_empty() && !e.date.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Escribe los eventos manuales en el archivo compartido (formato {id,title,date,course}).
pub fn write_user_events(evs: &[crate::store::CalEvent]) -> Result<(), String> {
    let arr: Vec<Value> = evs
        .iter()
        .map(|e| serde_json::json!({"id": e.id, "title": e.title, "date": e.date, "course": e.course_name}))
        .collect();
    let path = user_events_path();
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, serde_json::to_string(&arr).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Lanza una notificación de escritorio (libnotify / servidor de Quickshell).
pub fn notify(title: &str, body: &str) -> Result<(), String> {
    Command::new("notify-send")
        .args(["-a", "Aula", "-i", "x-office-calendar", title, body])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("notify-send: {}", e))
}

/// Lee la paleta Material You generada por matugen desde el wallpaper.
pub fn matugen_palette() -> Option<Value> {
    let path = home()
        .join(".local/state/quickshell/user/generated/colors.json");
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn concluded_set(state: &AppState) -> HashSet<u64> {
    state.courses.iter().filter(|c: &&Course| c.concluded).map(|c| c.id).collect()
}

/// Convierte ISO 8601 (2026-06-10T23:59:59Z) a formato básico iCal (20260610T235959Z).
fn ics_dt(iso: &str) -> String {
    let base = iso.trim().split('.').next().unwrap_or(iso);
    let mut out: String = base.chars().filter(|c| *c != '-' && *c != ':').collect();
    if !out.ends_with('Z') {
        out.push('Z');
    }
    out
}

fn ics_esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

/// Genera un calendario .ics (VEVENT por plazo + VTODO por tarea pendiente).
/// Excluí los ramos pasados para no llenar el calendario de plazos viejos.
pub fn export_ics(state: &AppState) -> Result<PathBuf, String> {
    let past = concluded_set(state);
    let mut s = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Aula//Gestor UC//ES\r\nCALSCALE:GREGORIAN\r\nX-WR-CALNAME:Aula UC\r\n",
    );
    for a in &state.assignments {
        if past.contains(&a.course_id) || a.user_status == "dismissed" {
            continue;
        }
        let due = match &a.due_at {
            Some(d) if !d.is_empty() => d,
            _ => continue,
        };
        let dt = ics_dt(due);
        let summary = ics_esc(&format!("{} · {}", a.name, a.course_name));
        // Evento (plazo)
        s.push_str("BEGIN:VEVENT\r\n");
        s.push_str(&format!("UID:aula-{}@aula\r\n", a.id));
        s.push_str(&format!("DTSTAMP:{}\r\n", dt));
        s.push_str(&format!("DTSTART:{}\r\n", dt));
        s.push_str(&format!("DTEND:{}\r\n", dt));
        s.push_str(&format!("SUMMARY:{}\r\n", summary));
        s.push_str(&format!("DESCRIPTION:{}\r\n", ics_esc(&a.course_name)));
        s.push_str("END:VEVENT\r\n");
        // Tarea pendiente (VTODO)
        let pending = !a.submitted && a.user_status != "done";
        if pending {
            s.push_str("BEGIN:VTODO\r\n");
            s.push_str(&format!("UID:aula-todo-{}@aula\r\n", a.id));
            s.push_str(&format!("DTSTAMP:{}\r\n", dt));
            s.push_str(&format!("DUE:{}\r\n", dt));
            s.push_str(&format!("SUMMARY:{}\r\n", summary));
            s.push_str("STATUS:NEEDS-ACTION\r\n");
            s.push_str("END:VTODO\r\n");
        }
    }
    // Eventos del calendario: detectados por IA (state.events) + manuales (archivo compartido).
    let manual = read_user_events();
    for ev in state.events.iter().chain(manual.iter()) {
        if ev.date.trim().is_empty() {
            continue;
        }
        let date_only = ev.date.len() == 10 && ev.date.contains('-');
        let (dtstart, stamp) = if date_only {
            let d = ev.date.replace('-', "");
            (format!("DTSTART;VALUE=DATE:{}", d), format!("{}T000000Z", d))
        } else {
            let dt = ics_dt(&ev.date);
            (format!("DTSTART:{}", dt), dt)
        };
        let titulo = if ev.course_name.is_empty() {
            ev.title.clone()
        } else {
            format!("{} · {}", ev.title, ev.course_name)
        };
        s.push_str("BEGIN:VEVENT\r\n");
        s.push_str(&format!("UID:aula-ev-{}@aula\r\n", ev.id));
        s.push_str(&format!("DTSTAMP:{}\r\n", stamp));
        s.push_str(&format!("{}\r\n", dtstart));
        s.push_str(&format!("SUMMARY:{}\r\n", ics_esc(&titulo)));
        s.push_str("END:VEVENT\r\n");
    }

    s.push_str("END:VCALENDAR\r\n");

    let dir = home().join(".local/share/aula");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("aula.ics");
    std::fs::write(&path, s).map_err(|e| e.to_string())?;

    // También un JSON sencillo para el widget de calendario de Quickshell.
    let _ = export_quickshell_events(state);
    Ok(path)
}

/// Escribe los eventos (plazos de tareas + detectados + manuales) en un JSON que
/// lee el widget de calendario de Quickshell. Formato: [{title, date:YYYY-MM-DD, course, kind}].
pub fn export_quickshell_events(state: &AppState) -> Result<(), String> {
    let past = concluded_set(state);
    let mut arr: Vec<Value> = Vec::new();
    for a in &state.assignments {
        if past.contains(&a.course_id) || a.user_status == "dismissed" {
            continue;
        }
        let due = match &a.due_at {
            Some(d) if d.len() >= 10 => d,
            _ => continue,
        };
        arr.push(serde_json::json!({
            "title": a.name,
            "date": local_date(due),
            "course": a.course_name,
            "kind": "tarea",
        }));
    }
    // Solo detectados/automáticos: los manuales los gestiona aula_user_events.json (Quickshell los lee aparte).
    for ev in &state.events {
        if ev.date.len() < 10 || ev.source == "manual" {
            continue;
        }
        arr.push(serde_json::json!({
            "title": ev.title,
            "date": &ev.date[..10],
            "course": ev.course_name,
            "kind": ev.source,
        }));
    }
    let dir = home().join(".local/state/quickshell/user");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("aula_events.json");
    std::fs::write(&path, serde_json::to_string(&arr).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

const TODO_MARK: &str = "🎓 ";

/// Sincroniza las tareas pendientes con la To Do de Quickshell (illogical-impulse).
/// Conserva las tareas propias del usuario; solo gestiona las que llevan el marcador.
pub fn sync_quickshell_todo(state: &AppState) -> Result<(), String> {
    let path = home().join(".local/state/quickshell/user/todo.json");
    if !path.exists() {
        return Ok(()); // No hay Quickshell To Do; no es error.
    }
    let past = concluded_set(state);
    // Cargar lista actual (preservando ítems del usuario).
    let mut list: Vec<Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // Quitar los ítems gestionados por Aula (los del marcador).
    list.retain(|it| {
        !it.get("content")
            .and_then(|c| c.as_str())
            .map(|c| c.starts_with(TODO_MARK))
            .unwrap_or(false)
    });
    // Añadir las tareas pendientes (fecha LOCAL correcta + campo `due`).
    // Omite las ya VENCIDAS: así se "auto-borran" (no se vuelven a añadir).
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    for a in &state.assignments {
        if past.contains(&a.course_id) || a.submitted || a.user_status == "done" || a.user_status == "dismissed" {
            continue;
        }
        let due = a.due_at.as_deref().unwrap_or("");
        if due.is_empty() {
            continue;
        }
        let fecha = local_date(due);
        if fecha < today {
            continue; // plazo pasado
        }
        let content = format!("{}{} · {} (vence {})", TODO_MARK, a.name, a.course_name, fecha);
        list.push(serde_json::json!({ "content": content, "done": false, "due": fecha }));
    }
    std::fs::write(&path, serde_json::to_string(&list).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
