use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuración de Aula. Las API keys cloud se reutilizan de Nexo
/// (~/.config/cortado/config.json) para no duplicarlas.
#[derive(Serialize, Deserialize, Clone)]
pub struct AppConfig {
    /// Instancia Canvas de la universidad.
    #[serde(default = "default_canvas_url")]
    pub canvas_url: String,
    /// Token de acceso personal de Canvas (Cuenta → Configuración → Nuevo token).
    #[serde(default)]
    pub canvas_token: String,
    /// Carpeta donde se descargan y organizan los archivos.
    #[serde(default = "default_download_dir")]
    pub download_dir: String,
    /// Fecha (ISO) en que se guardó el token de Canvas. Caduca a los 120 días.
    #[serde(default)]
    pub token_created: String,
    /// URL de Ollama (IA local: clasificación, embeddings, OCR).
    #[serde(default = "default_ollama")]
    pub ollama_url: String,
    /// Modelo local para clasificar documentos.
    #[serde(default = "default_classify_model")]
    pub classify_model: String,
    /// Modelo local para embeddings (índice semántico).
    #[serde(default = "default_embed_model")]
    pub embed_model: String,
    /// Proveedor cloud para resúmenes (gemini | openrouter).
    #[serde(default = "default_summary_provider")]
    pub summary_provider: String,
    /// Modelo cloud para resúmenes.
    #[serde(default = "default_summary_model")]
    pub summary_model: String,
    /// Si la estética sigue los colores del wallpaper (matugen).
    /// Por defecto NO: se usa el tema fijo "Voltaic".
    #[serde(default)]
    pub dynamic_theme: bool,
}

fn default_canvas_url() -> String { "https://cursos.canvas.uc.cl".into() }
fn default_download_dir() -> String { "/mnt/linux/Universidad".into() }
fn default_ollama() -> String { "http://localhost:11434".into() }
fn default_classify_model() -> String { "qwen2.5:7b".into() }
fn default_embed_model() -> String { "nomic-embed-text:latest".into() }
fn default_summary_provider() -> String { "gemini".into() }
// gemini-3.5-flash: el modelo más nuevo y capaz disponible en el tier gratuito
// de la clave (los -preview y los *-pro requieren facturación de pago → 429).
fn default_summary_model() -> String { "gemini-3.5-flash".into() }

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            canvas_url: default_canvas_url(),
            canvas_token: String::new(),
            download_dir: default_download_dir(),
            token_created: String::new(),
            ollama_url: default_ollama(),
            classify_model: default_classify_model(),
            embed_model: default_embed_model(),
            summary_provider: default_summary_provider(),
            summary_model: default_summary_model(),
            dynamic_theme: false,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("aula")
        .join("config.json")
}

pub fn load() -> AppConfig {
    match std::fs::read_to_string(config_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

pub fn save(cfg: &AppConfig) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, data).map_err(|e| e.to_string())?;
    // Permisos 600: guarda el token de Canvas.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Claves cloud importadas de Nexo (cortado). Devuelve (openrouter, gemini).
pub fn nexo_keys() -> (String, String) {
    let path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("cortado")
        .join("config.json");
    if let Ok(s) = std::fs::read_to_string(path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&s) {
            let keys = json.get("keys");
            let get = |k: &str| {
                keys.and_then(|o| o.get(k))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            return (get("openrouter"), get("gemini"));
        }
    }
    (String::new(), String::new())
}
