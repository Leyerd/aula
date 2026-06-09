use crate::config::AppConfig;
use crate::store::{Announcement, Assignment, CanvasFile, Course};
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

fn api_base(cfg: &AppConfig) -> String {
    format!("{}/api/v1", cfg.canvas_url.trim_end_matches('/'))
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("Aula/0.1 (Gestor Universitario UC)")
        .build()
        .unwrap_or_default()
}

/// Extrae la URL `rel="next"` de la cabecera Link de Canvas.
fn next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let link = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    for part in link.split(',') {
        if part.contains("rel=\"next\"") {
            let start = part.find('<')? + 1;
            let end = part.find('>')?;
            return Some(part[start..end].to_string());
        }
    }
    None
}

/// GET paginado: sigue los enlaces `next` hasta agotar. `start` es URL absoluta.
async fn get_all(cfg: &AppConfig, start: String) -> Result<Vec<Value>, String> {
    let cl = client();
    let mut out: Vec<Value> = Vec::new();
    let mut url = Some(start);
    while let Some(u) = url {
        let resp = cl
            .get(&u)
            .bearer_auth(&cfg.canvas_token)
            .send()
            .await
            .map_err(|e| format!("Conexión Canvas: {}", e))?;
        if !resp.status().is_success() {
            let s = resp.status();
            if s.as_u16() == 401 {
                return Err("Token rechazado o caducado (401). Genera uno nuevo en Canvas → Cuenta → Configuración.".into());
            }
            let t = resp.text().await.unwrap_or_default();
            return Err(format!("HTTP {}: {}", s, t.chars().take(200).collect::<String>()));
        }
        let next = next_link(resp.headers());
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        match json {
            Value::Array(arr) => out.extend(arr),
            other => out.push(other),
        }
        url = next;
    }
    Ok(out)
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// Verifica el token y devuelve el nombre del usuario.
pub async fn verify(cfg: &AppConfig) -> Result<String, String> {
    if cfg.canvas_token.trim().is_empty() {
        return Err("Falta el token de Canvas (Ajustes).".into());
    }
    let url = format!("{}/users/self/profile", api_base(cfg));
    let resp = client()
        .get(&url)
        .bearer_auth(&cfg.canvas_token)
        .send()
        .await
        .map_err(|e| format!("Conexión: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Token rechazado (HTTP {}). Revisa la URL y el token.", resp.status()));
    }
    let json: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(s(&json, "name"))
}

fn course_from_json(c: &Value, concluded: bool) -> Option<Course> {
    let id = c.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
    if id == 0 {
        return None;
    }
    // Cursos restringidos por fecha sin nombre no aportan nada → fuera.
    if s(c, "name").is_empty() {
        return None;
    }
    let term = c
        .get("term")
        .and_then(|t| t.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    Some(Course {
        id,
        name: s(c, "name"),
        code: s(c, "course_code"),
        term,
        concluded,
    })
}

/// Lista TODOS los ramos: primero los activos, luego los pasados (concluded=true).
/// Los pasados tienen menor prioridad de descarga/resumen.
pub async fn fetch_courses(cfg: &AppConfig) -> Result<Vec<Course>, String> {
    let base = api_base(cfg);
    // Activos (obligatorio: si esto falla, el token es inválido).
    let active_url = format!("{}/courses?enrollment_state=active&per_page=100&include[]=term", base);
    let active = get_all(cfg, active_url).await?;

    let mut courses: Vec<Course> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for c in &active {
        if let Some(course) = course_from_json(c, false) {
            seen.insert(course.id);
            courses.push(course);
        }
    }

    // Pasados (completed). Best-effort: no aborta si falla.
    let past_url = format!("{}/courses?enrollment_state=completed&per_page=100&include[]=term", base);
    if let Ok(past) = get_all(cfg, past_url).await {
        for c in &past {
            if let Some(course) = course_from_json(c, true) {
                if seen.insert(course.id) {
                    courses.push(course);
                }
            }
        }
    }

    // Refuerzo: descubrir TODOS los ramos cursados vía el endpoint de matrículas
    // (el listado /courses a veces omite ramos antiguos/concluidos).
    let enr_url = format!(
        "{}/users/self/enrollments?per_page=100&state[]=active&state[]=completed&state[]=inactive",
        base
    );
    if let Ok(enrolls) = get_all(cfg, enr_url).await {
        let mut missing: Vec<u64> = Vec::new();
        for e in &enrolls {
            if let Some(cid) = e.get("course_id").and_then(|v| v.as_u64()) {
                if !seen.contains(&cid) && !missing.contains(&cid) {
                    missing.push(cid);
                }
            }
        }
        for cid in missing {
            let one_url = format!("{}/courses/{}?include[]=term", base, cid);
            if let Ok(mut arr) = get_all(cfg, one_url).await {
                if let Some(c) = arr.pop() {
                    // No estaba en activos → tratar como pasado.
                    if let Some(course) = course_from_json(&c, true) {
                        if seen.insert(course.id) {
                            courses.push(course);
                        }
                    }
                }
            }
        }
    }
    Ok(courses)
}

/// Tareas/evaluaciones de un ramo, con estado de entrega.
pub async fn fetch_assignments(cfg: &AppConfig, course: &Course) -> Result<Vec<Assignment>, String> {
    let url = format!(
        "{}/courses/{}/assignments?per_page=100&include[]=submission",
        api_base(cfg),
        course.id
    );
    let raw = get_all(cfg, url).await.unwrap_or_default();
    let mut out = Vec::new();
    for a in raw {
        let id = a.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        let submission = a.get("submission");
        let state = submission
            .and_then(|x| x.get("workflow_state"))
            .and_then(|x| x.as_str())
            .unwrap_or("unsubmitted")
            .to_string();
        let submitted = submission
            .and_then(|x| x.get("submitted_at"))
            .map(|x| !x.is_null())
            .unwrap_or(false)
            || state == "submitted"
            || state == "graded";
        out.push(Assignment {
            id,
            course_id: course.id,
            course_name: course.name.clone(),
            name: s(&a, "name"),
            due_at: a.get("due_at").and_then(|x| x.as_str()).map(|x| x.to_string()),
            points: a.get("points_possible").and_then(|x| x.as_f64()),
            html_url: s(&a, "html_url"),
            submitted,
            state,
            user_status: String::new(),
        });
    }
    Ok(out)
}

/// Limpia HTML a texto plano (los anuncios vienen en HTML).
fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
        .replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ")
        .split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Anuncios de un ramo (pestaña Anuncios = discussion_topics con only_announcements).
/// La IA los lee para detectar fechas importantes que a veces SOLO se avisan aquí.
/// Best-effort: si falla, devuelve vacío.
pub async fn fetch_announcements(cfg: &AppConfig, course: &Course) -> Vec<Announcement> {
    let url = format!(
        "{}/courses/{}/discussion_topics?only_announcements=true&per_page=50",
        api_base(cfg),
        course.id
    );
    let raw = get_all(cfg, url).await.unwrap_or_default();
    let mut out = Vec::new();
    for a in raw {
        let id = a.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        if id == 0 {
            continue;
        }
        out.push(Announcement {
            id,
            course_id: course.id,
            course_name: course.name.clone(),
            title: s(&a, "title"),
            message: strip_html(&s(&a, "message")),
            posted_at: s(&a, "posted_at"),
            dates_done: false,
        });
    }
    out
}

/// Mapa folder_id -> ruta legible ("course files/Clases/...").
async fn folder_map(cfg: &AppConfig, course_id: u64) -> HashMap<u64, String> {
    let url = format!("{}/courses/{}/folders?per_page=100", api_base(cfg), course_id);
    let mut map = HashMap::new();
    if let Ok(raw) = get_all(cfg, url).await {
        for f in raw {
            if let Some(id) = f.get("id").and_then(|v| v.as_u64()) {
                let name = s(&f, "full_name");
                map.insert(id, name);
            }
        }
    }
    map
}

fn file_from_json(f: &Value, course: &Course, folders: &HashMap<u64, String>) -> Option<CanvasFile> {
    let id = f.get("id").and_then(|v| v.as_u64())?;
    let filename = {
        let dn = s(f, "display_name");
        if dn.is_empty() { s(f, "filename") } else { dn }
    };
    if filename.is_empty() {
        return None;
    }
    let folder = f
        .get("folder_id")
        .and_then(|v| v.as_u64())
        .and_then(|fid| folders.get(&fid).cloned())
        .unwrap_or_default();
    let content_type = {
        let ct = s(f, "content-type");
        if ct.is_empty() { s(f, "content_type") } else { ct }
    };
    Some(CanvasFile {
        id,
        course_id: course.id,
        course_name: course.name.clone(),
        folder,
        filename,
        url: s(f, "url"),
        size: f.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        content_type,
        ..Default::default()
    })
}

/// Archivos de un ramo. Si la pestaña Archivos está deshabilitada,
/// recurre a los módulos del curso.
pub async fn fetch_files(cfg: &AppConfig, course: &Course) -> Result<Vec<CanvasFile>, String> {
    let folders = folder_map(cfg, course.id).await;
    let url = format!("{}/courses/{}/files?per_page=100", api_base(cfg), course.id);
    match get_all(cfg, url).await {
        Ok(raw) if !raw.is_empty() => {
            Ok(raw.iter().filter_map(|f| file_from_json(f, course, &folders)).collect())
        }
        _ => fetch_files_via_modules(cfg, course, &folders).await,
    }
}

/// Fallback: recorre módulos y resuelve los ítems de tipo File.
async fn fetch_files_via_modules(
    cfg: &AppConfig,
    course: &Course,
    folders: &HashMap<u64, String>,
) -> Result<Vec<CanvasFile>, String> {
    let url = format!(
        "{}/courses/{}/modules?include[]=items&per_page=100",
        api_base(cfg),
        course.id
    );
    let modules = get_all(cfg, url).await.unwrap_or_default();
    let cl = client();
    let mut out = Vec::new();
    for m in modules {
        let items = match m.get("items").and_then(|i| i.as_array()) {
            Some(i) => i,
            None => continue,
        };
        for it in items {
            if s(it, "type") != "File" {
                continue;
            }
            // El ítem trae 'url' = endpoint API del archivo.
            let api_url = s(it, "url");
            if api_url.is_empty() {
                continue;
            }
            if let Ok(resp) = cl.get(&api_url).bearer_auth(&cfg.canvas_token).send().await {
                if let Ok(fj) = resp.json::<Value>().await {
                    if let Some(cf) = file_from_json(&fj, course, folders) {
                        out.push(cf);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Descarga un archivo a `dest`. Crea las carpetas necesarias.
pub async fn download_file(cfg: &AppConfig, file: &CanvasFile, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let resp = client()
        .get(&file.url)
        .bearer_auth(&cfg.canvas_token)
        .send()
        .await
        .map_err(|e| format!("Descarga: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} al descargar {}", resp.status(), file.filename));
    }
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| e.to_string())?;
        buf.extend_from_slice(&bytes);
    }
    std::fs::write(dest, &buf).map_err(|e| e.to_string())
}
