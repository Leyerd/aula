use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Un ramo (curso de Canvas).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Course {
    pub id: u64,
    pub name: String,
    pub code: String,
    pub term: String,
    /// true si el curso ya terminó (matrícula completada). Baja prioridad.
    #[serde(default)]
    pub concluded: bool,
}

/// Una tarea/evaluación con fecha de entrega.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Assignment {
    pub id: u64,
    pub course_id: u64,
    pub course_name: String,
    pub name: String,
    pub due_at: Option<String>,
    pub points: Option<f64>,
    pub html_url: String,
    /// true si ya hay entrega registrada.
    pub submitted: bool,
    /// submission workflow_state (unsubmitted | submitted | graded | pending_review).
    pub state: String,
    /// Override manual del usuario: "" | "done" (hecha) | "dismissed" (descartada).
    #[serde(default)]
    pub user_status: String,
}

/// Un archivo de un ramo.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CanvasFile {
    pub id: u64,
    pub course_id: u64,
    pub course_name: String,
    /// Carpeta relativa dentro del ramo (según Canvas).
    pub folder: String,
    pub filename: String,
    pub url: String,
    pub size: u64,
    pub content_type: String,
    /// Ruta local una vez descargado.
    #[serde(default)]
    pub local_path: String,
    /// Categoría asignada por la IA.
    #[serde(default)]
    pub category: String,
    /// true si ya se clasificó leyendo su CONTENIDO (no solo el nombre).
    #[serde(default)]
    pub content_done: bool,
    /// true si el usuario fijó la categoría a mano (no se auto-reclasifica).
    #[serde(default)]
    pub category_manual: bool,
    /// Resumen generado por la IA cloud (markdown).
    #[serde(default)]
    pub summary: String,
    /// Vector de embedding para búsqueda semántica.
    #[serde(default)]
    pub embedding: Vec<f32>,
    /// Ruta a un PDF del resumen (generado a partir del summary). Se preserva en sync.
    #[serde(default)]
    pub summary_pdf: String,
}

/// Resumen GENERAL de un ramo para una categoría (compilado de todas sus clases,
/// ayudantías, etc.). Se genera solo si el ramo terminó o a petición del usuario.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CourseSummary {
    pub course_id: u64,
    pub course_name: String,
    pub category: String,
    pub summary: String,
}

/// Evento de calendario: detectado por IA en un documento ("detected") o
/// añadido a mano por el usuario ("manual").
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CalEvent {
    pub id: String,
    pub title: String,
    /// Fecha ISO (YYYY-MM-DD) o fecha-hora ISO.
    pub date: String,
    #[serde(default)]
    pub course_name: String,
    #[serde(default)]
    pub source: String, // "detected" | "manual"
}

/// Un anuncio de un ramo (Canvas). La IA lo lee para detectar fechas importantes
/// (pruebas, entregas, cambios de horario) que muchas veces SOLO se avisan aquí.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Announcement {
    pub id: u64,
    pub course_id: u64,
    pub course_name: String,
    pub title: String,
    /// Cuerpo del anuncio en texto plano (HTML limpiado).
    pub message: String,
    pub posted_at: String,
    /// true si la IA ya extrajo fechas de este anuncio (no reprocesar).
    #[serde(default)]
    pub dates_done: bool,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AppState {
    pub courses: Vec<Course>,
    pub assignments: Vec<Assignment>,
    pub files: Vec<CanvasFile>,
    #[serde(default)]
    pub course_summaries: Vec<CourseSummary>,
    #[serde(default)]
    pub events: Vec<CalEvent>,
    /// Anuncios de los ramos (para que la IA detecte fechas).
    #[serde(default)]
    pub announcements: Vec<Announcement>,
    /// Última sincronización (ISO 8601, fijada por el frontend).
    #[serde(default)]
    pub last_sync: String,
    /// Claves de avisos ya enviados: "<assignment_id>@<umbral>" (evita repetir).
    #[serde(default)]
    pub notified: Vec<String>,
}

pub fn state_path(download_dir: &str) -> PathBuf {
    Path::new(download_dir).join(".aula").join("state.json")
}

pub fn load(download_dir: &str) -> AppState {
    match std::fs::read_to_string(state_path(download_dir)) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => AppState::default(),
    }
}

pub fn save(download_dir: &str, state: &AppState) -> Result<(), String> {
    let path = state_path(download_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(&path, data).map_err(|e| e.to_string())
}
