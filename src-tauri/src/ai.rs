use crate::config::{self, AppConfig};
use serde_json::Value;
use std::path::Path;

/// Categorías en las que la IA local clasifica cada documento
/// (nomenclatura típica de la UC).
pub const CATEGORIES: &[&str] = &[
    "Clase",
    "Ayudantía",
    "Cápsula",
    "Laboratorio",
    "Guía/Ejercicios",
    "Tarea",
    "Prueba/Control",
    "Lectura/Paper",
    "Programa/Syllabus",
    "Administrativo",
    "Otro",
];

/// Categorías cuyo material tiene sentido resumir.
pub fn is_summarizable(cat: &str) -> bool {
    matches!(
        cat,
        "Clase" | "Ayudantía" | "Cápsula" | "Laboratorio" | "Guía/Ejercicios"
            | "Tarea" | "Prueba/Control" | "Lectura/Paper"
    )
}

/// Clasifica SOLO por el nombre del archivo (instantáneo, sin LLM ni descarga).
#[allow(dead_code)]
pub fn classify_by_name(filename: &str) -> String {
    keyword_category(filename).to_string()
}

/// Heurística por palabras clave (nombre de archivo + texto). Respaldo del LLM.
fn keyword_category(hay: &str) -> &'static str {
    let n = hay.to_lowercase();
    let has = |k: &str| n.contains(k);
    if has("ayudant") {
        "Ayudantía"
    } else if has("laborator") || has("lab_") || has("lab-") || has("lab ") || n.starts_with("lab") {
        "Laboratorio"
    } else if has("capsula") || has("cápsula") {
        "Cápsula"
    } else if has("pauta") || has("control") || has("certamen") || has("certámen")
        || has("interrogaci") || has("examen") || has("prueba") || has("solemne")
        || has(" i1") || has(" i2") || has(" i3")
    {
        "Prueba/Control"
    } else if has("tarea") || has("entrega") || has("homework") || has("assignment") {
        "Tarea"
    } else if has("guia") || has("guía") || has("ejercicio") || has("problema") || has("taller") {
        "Guía/Ejercicios"
    } else if has("paper") || has("lectura") || has("reading") || has("articulo") || has("artículo") {
        "Lectura/Paper"
    } else if has("programa") || has("syllabus") || has("temario") || has("calendario") || has("reglamento") {
        "Programa/Syllabus"
    } else if has("clase") || has("apunte") || has("lecture") || has("catedra") || has("cátedra")
        || has("slide") || has("presentaci")
    {
        "Clase"
    } else {
        "Otro"
    }
}

/// Clasificación DETERMINISTA por la carpeta de Canvas (la señal más fiable).
/// Devuelve None si la carpeta es genérica/ambigua (entonces decide la IA por contenido).
pub fn folder_category(folder: &str) -> Option<&'static str> {
    let last = folder.rsplit('/').next().unwrap_or(folder).trim().to_lowercase();
    // Carpetas genéricas → ambiguo.
    if matches!(last.as_str(), "" | "course files" | "unfiled" | "files" | "material"
        | "materiales" | "documentos" | "documents" | "general" | "varios" | "otros") {
        return None;
    }
    let has = |k: &str| last.contains(k);
    if has("ayudant") {
        Some("Ayudantía")
    } else if has("laborator") || last == "lab" || last == "labs" {
        Some("Laboratorio")
    } else if has("capsula") || has("cápsula") {
        Some("Cápsula")
    } else if has("control") || has("prueba") || has("certamen") || has("certámen")
        || has("interrogac") || has("examen") || has("solemne") || has("evaluac") || has("pauta") {
        Some("Prueba/Control")
    } else if has("tarea") || has("entrega") {
        Some("Tarea")
    } else if has("guia") || has("guía") || has("ejercicio") || has("taller") || has("problema") {
        Some("Guía/Ejercicios")
    } else if has("clase") || has("catedra") || has("cátedra") || has("apunte")
        || has("teoria") || has("teoría") || has("slide") || has("presentaci") || has("compilad") {
        Some("Clase")
    } else if has("lectura") || has("paper") || has("bibliograf") || has("reading")
        || has("articulo") || has("artículo") {
        Some("Lectura/Paper")
    } else if has("programa") || has("syllabus") || has("temario") || has("reglamento") || has("calendario") {
        Some("Programa/Syllabus")
    } else {
        None
    }
}

/// Extrae texto plano de un archivo (PDF, txt, md, csv). Trunca a `max_chars`.
/// Devuelve cadena vacía si el formato no es texto extraíble.
pub fn extract_text(path: &str, max_chars: usize) -> String {
    let p = Path::new(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let text = match ext.as_str() {
        // pdftotext (poppler) es ROBUSTO; pdf-extract panickea con muchos PDFs.
        // Usamos pdftotext y solo caemos a pdf-extract (aislado) si falla.
        "pdf" => {
            let out = std::process::Command::new("pdftotext")
                .args(["-q", "-l", "40", path, "-"])
                .output();
            match out {
                Ok(o) if o.status.success() && !o.stdout.is_empty() => {
                    String::from_utf8_lossy(&o.stdout).to_string()
                }
                _ => {
                    let pb = p.to_path_buf();
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        pdf_extract::extract_text(&pb).unwrap_or_default()
                    }))
                    .unwrap_or_default()
                }
            }
        }
        "txt" | "md" | "markdown" | "csv" | "tex" | "json" | "rs" | "py" | "ts" | "js" => {
            std::fs::read_to_string(p).unwrap_or_default()
        }
        _ => String::new(),
    };
    let trimmed: String = text.chars().take(max_chars).collect();
    trimmed.trim().to_string()
}

/// Nº máximo de páginas que se envían como imagen a la IA multimodal.
/// Acota el coste/tamaño de la petición; el texto extraído cubre el resto.
const MAX_PDF_IMAGES: usize = 24;

/// Renderiza las primeras `max_pages` páginas de un PDF a PNG y las devuelve
/// como data-URIs (`data:image/png;base64,…`), listas para el endpoint
/// multimodal de Gemini. Permite que la IA VEA ejercicios, figuras y fórmulas
/// que el texto plano (pdftotext) pierde. Falla en silencio (vector vacío).
pub fn render_pdf_images(path: &str, max_pages: usize) -> Vec<String> {
    use base64::Engine;
    let p = Path::new(path);
    if p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()) != Some("pdf".into()) {
        return Vec::new();
    }
    // Carpeta temporal única (pid + nanos) para no pisar otras conversiones.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("aula_mm_{}_{}", std::process::id(), nanos));
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    let prefix = dir.join("p");
    // pdftoppm: PNG, ~120 dpi, escalado a 1240px de ancho (≈77 KB/página en base64).
    let out = std::process::Command::new("pdftoppm")
        .args([
            "-png", "-r", "120", "-scale-to-x", "1240", "-scale-to-y", "-1",
            "-f", "1", "-l", &max_pages.to_string(), path,
            prefix.to_str().unwrap_or("p"),
        ])
        .output();
    let mut imgs = Vec::new();
    if matches!(out, Ok(ref o) if o.status.success()) {
        // Recolecta los PNG generados en orden de página.
        let mut pages: Vec<_> = std::fs::read_dir(&dir)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("png"))
                .collect())
            .unwrap_or_default();
        pages.sort();
        for page in pages.iter().take(max_pages) {
            if let Ok(bytes) = std::fs::read(page) {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                imgs.push(format!("data:image/png;base64,{}", b64));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    imgs
}

/// Clasifica un documento en una de las CATEGORIES usando Ollama (local),
/// con respaldo por palabras clave del nombre + contenido.
#[allow(dead_code)]
pub async fn classify(cfg: &AppConfig, filename: &str, text: &str) -> Result<String, String> {
    let opciones = CATEGORIES.join(" | ");
    let muestra: String = text.chars().take(3000).collect();
    let prompt = format!(
        "Clasifica este material universitario (Pontificia U. Católica de Chile) en UNA categoría.\n\
         Definiciones:\n\
         - Clase: apuntes/slides de cátedra, materia teórica.\n\
         - Ayudantía: sesión de ayudante, normalmente con ejercicios y su resolución.\n\
         - Cápsula: material breve/complementario (mini-vídeo o resumen corto).\n\
         - Laboratorio: guía, protocolo o informe de laboratorio/práctico.\n\
         - Guía/Ejercicios: listado de problemas o taller para practicar.\n\
         - Tarea: trabajo evaluado a entregar.\n\
         - Prueba/Control: control, interrogación (I1/I2), certamen, examen o su pauta.\n\
         - Lectura/Paper: artículo o lectura obligatoria.\n\
         - Programa/Syllabus: programa del curso, reglamento, calendario.\n\
         - Administrativo / Otro.\n\n\
         Categorías válidas: {opciones}\n\n\
         Nombre del archivo: {filename}\n\
         Contenido (extracto):\n{muestra}\n\n\
         Responde SOLO con el nombre exacto de la categoría."
    );
    let hay = format!("{} {}", filename, muestra);
    match ollama_chat(cfg, &cfg.classify_model, &prompt).await {
        Ok(raw) => {
            let ans = raw.to_lowercase();
            for c in CATEGORIES {
                if ans.contains(&c.to_lowercase()) {
                    return Ok(c.to_string());
                }
            }
            // El LLM respondió algo no exacto → heurística sobre su respuesta + archivo.
            Ok(keyword_category(&format!("{} {}", hay, ans)).to_string())
        }
        // Ollama caído → al menos clasificar por nombre.
        Err(_) => Ok(keyword_category(&hay).to_string()),
    }
}

fn extract_json_array(s: &str) -> Option<Vec<serde_json::Value>> {
    let start = s.find('[')?;
    let end = s.rfind(']')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Vec<serde_json::Value>>(&s[start..=end]).ok()
}

fn match_category(s: &str) -> Option<String> {
    let l = s.to_lowercase();
    CATEGORIES.iter().find(|c| l.contains(&c.to_lowercase())).map(|c| c.to_string())
}

/// Clasifica un LOTE de archivos con la IA local usando nombre + carpeta + ramo
/// (sin necesidad de descargarlos). Devuelve una categoría por archivo, alineada
/// con `items`. Si la IA falla, cae a la heurística por nombre+carpeta.
pub async fn classify_batch(cfg: &AppConfig, items: &[(String, String, String)]) -> Vec<String> {
    // Respaldo: nombre + carpeta (la carpeta de Canvas suele ser muy indicativa).
    let mut out: Vec<String> = items
        .iter()
        .map(|(f, folder, _)| keyword_category(&format!("{} {}", folder, f)).to_string())
        .collect();

    let opciones = CATEGORIES.join(" | ");
    let mut listado = String::new();
    for (i, (fname, folder, course)) in items.iter().enumerate() {
        listado.push_str(&format!("{}. archivo=\"{}\" | carpeta=\"{}\" | ramo=\"{}\"\n", i, fname, folder, course));
    }
    let prompt = format!(
        "Clasifica CADA material universitario (Pontificia U. Católica de Chile) en UNA categoría.\n\
         Pistas: la carpeta de Canvas suele indicar el tipo (Ayudantías, Clases, Controles…). \
         I1/I2/I3, certamen, control, solemne, pauta → Prueba/Control. Capsula → Cápsula.\n\
         Categorías válidas (usa el nombre EXACTO): {opciones}\n\n\
         Archivos:\n{listado}\n\
         Responde SOLO un objeto JSON con la clave \"items\": un array con una entrada por archivo, así: \
         {{\"items\":[{{\"i\":0,\"cat\":\"Clase\"}}, {{\"i\":1,\"cat\":\"Ayudantía\"}}]}}"
    );
    // La nube (Gemini) clasifica mucho mejor. Si no hay clave o falla,
    // se conserva la heurística carpeta+nombre (más fiable que el 7B local para esto).
    if let Ok(text) = cloud_chat(cfg, &prompt, 0.0, true).await {
        if let Some(arr) = extract_json_array(&text) {
            for entry in arr {
                let i = entry.get("i").and_then(|v| v.as_u64()).map(|x| x as usize);
                let cat = entry.get("cat").and_then(|v| v.as_str()).and_then(match_category);
                if let (Some(i), Some(cat)) = (i, cat) {
                    if i < out.len() {
                        out[i] = cat;
                    }
                }
            }
        }
    }
    out
}

/// Resultado de clasificar un documento por contenido.
pub struct ClassResult {
    pub category: String,
    /// Fecha importante detectada (ISO YYYY-MM-DD) o vacío.
    pub date: String,
    /// Título del evento/plazo detectado o vacío.
    pub event: String,
}

/// Clasifica un LOTE de archivos DESCARGADOS leyendo un extracto de su CONTENIDO
/// y además DETECTA fechas importantes (pruebas, entregas) mencionadas en el texto.
/// items = (nombre, carpeta, ramo, extracto).
pub async fn classify_batch_content(cfg: &AppConfig, items: &[(String, String, String, String)]) -> Vec<ClassResult> {
    let mut out: Vec<ClassResult> = items
        .iter()
        .map(|(f, folder, _, _)| ClassResult {
            category: keyword_category(&format!("{} {}", folder, f)).to_string(),
            date: String::new(),
            event: String::new(),
        })
        .collect();
    let opciones = CATEGORIES.join(" | ");
    let mut listado = String::new();
    for (i, (fname, folder, course, excerpt)) in items.iter().enumerate() {
        let ex: String = excerpt.chars().take(1400).collect();
        listado.push_str(&format!(
            "### {} | archivo=\"{}\" | carpeta=\"{}\" | ramo=\"{}\"\nExtracto del contenido:\n{}\n\n",
            i, fname, folder, course, ex
        ));
    }
    let prompt = format!(
        "Para CADA documento universitario (PUC Chile): (a) clasifícalo en UNA categoría por su CONTENIDO \
         (el nombre puede no decir el tipo), y (b) si el texto menciona una FECHA IMPORTANTE (fecha de una prueba/control/certamen, \
         o de entrega de una tarea), extráela.\n\
         Categorías (nombre EXACTO): {opciones}\n\
         Pistas: Clase=materia/slides; Ayudantía=ejercicios resueltos; Laboratorio=guía/informe de práctico; Guía/Ejercicios=problemas; Tarea=trabajo a entregar; \
         Prueba/Control=I1/I2/certamen/control/examen/pauta.\n\n\
         Documentos:\n{listado}\n\
         Responde SOLO un objeto JSON con la clave \"items\": un array con una entrada por documento, con este formato exacto:\n\
         {{\"items\":[{{\"i\":0,\"cat\":\"Clase\",\"fecha\":\"\",\"evento\":\"\"}}, {{\"i\":1,\"cat\":\"Tarea\",\"fecha\":\"2026-05-20\",\"evento\":\"Entrega Tarea 2\"}}]}}\n\
         Usa \"fecha\" en formato YYYY-MM-DD solo si estás seguro; si no hay fecha clara, deja \"fecha\":\"\" y \"evento\":\"\"."
    );
    if let Ok(text) = cloud_chat(cfg, &prompt, 0.0, true).await {
        if let Some(arr) = extract_json_array(&text) {
            for entry in arr {
                let i = match entry.get("i").and_then(|v| v.as_u64()).map(|x| x as usize) {
                    Some(i) if i < out.len() => i,
                    _ => continue,
                };
                if let Some(cat) = entry.get("cat").and_then(|v| v.as_str()).and_then(match_category) {
                    out[i].category = cat;
                }
                let fecha = entry.get("fecha").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                // Aceptar solo fechas con pinta de ISO (YYYY-MM-DD).
                if fecha.len() >= 8 && fecha.chars().take(4).all(|c| c.is_ascii_digit()) && fecha.contains('-') {
                    out[i].date = fecha;
                    out[i].event = entry.get("evento").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                }
            }
        }
    }
    out
}

/// Lee un LOTE de anuncios y EXTRAE todas las fechas importantes que mencionen
/// (pruebas, entregas, ayudantías especiales, plazos, cambios de horario).
/// items = (titulo, ramo, texto). Devuelve, por anuncio, una lista de (fecha ISO, evento).
pub async fn detect_dates(cfg: &AppConfig, items: &[(String, String, String)]) -> Vec<Vec<(String, String)>> {
    let mut out: Vec<Vec<(String, String)>> = items.iter().map(|_| Vec::new()).collect();
    if items.is_empty() {
        return out;
    }
    let mut listado = String::new();
    for (i, (title, course, text)) in items.iter().enumerate() {
        let ex: String = text.chars().take(2000).collect();
        listado.push_str(&format!("### {} | titulo=\"{}\" | ramo=\"{}\"\n{}\n\n", i, title, course, ex));
    }
    let prompt = format!(
        "Eres un asistente académico (Pontificia U. Católica de Chile). Para CADA anuncio, EXTRAE todas las \
         FECHAS IMPORTANTES que mencione: fecha de prueba/control/certamen/interrogación/examen, entrega de \
         tarea/proyecto, ayudantía o clase especial, plazo o cambio de horario. Si un anuncio no tiene una fecha \
         clara, devuelve lista vacía para él. Da la fecha en formato YYYY-MM-DD (deduce el año del contexto; \
         si no se sabe, usa el año en curso). El 'evento' debe ser una etiqueta breve (ej. 'Control 2', 'Entrega Proyecto').\n\n\
         Anuncios:\n{listado}\n\
         Responde SOLO un objeto JSON con la clave \"items\": un array con una entrada por anuncio EN ORDEN, así:\n\
         {{\"items\":[{{\"i\":0,\"fechas\":[{{\"fecha\":\"2026-05-20\",\"evento\":\"Control 2\"}}]}}, {{\"i\":1,\"fechas\":[]}}]}}"
    );
    if let Ok(text) = cloud_chat(cfg, &prompt, 0.0, true).await {
        if let Some(arr) = extract_json_array(&text) {
            for entry in arr {
                let i = match entry.get("i").and_then(|v| v.as_u64()).map(|x| x as usize) {
                    Some(i) if i < out.len() => i,
                    _ => continue,
                };
                if let Some(fechas) = entry.get("fechas").and_then(|v| v.as_array()) {
                    for f in fechas {
                        let fecha = f.get("fecha").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                        if fecha.len() >= 8 && fecha.chars().take(4).all(|c| c.is_ascii_digit()) && fecha.contains('-') {
                            let evento = f.get("evento").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                            out[i].push((fecha, evento));
                        }
                    }
                }
            }
        }
    }
    out
}

/// Resume un documento con la mejor IA cloud disponible (vía claves de Nexo).
/// El estilo del resumen depende de la categoría: las clases se resumen para
/// estudiar; las ayudantías/guías se resuelven PASO A PASO; etc.
/// CONTEXTO LIMPIO: cada llamada es una petición independiente con un único
/// mensaje de usuario (no hay historial compartido), así que un resumen NUNCA
/// mezcla información de otro archivo.
pub async fn summarize(cfg: &AppConfig, filename: &str, category: &str, text: &str, path: &str) -> Result<String, String> {
    // VISIÓN MULTIMODAL: renderizamos las páginas del PDF a imagen para que la IA
    // VEA ejercicios, figuras y fórmulas (lo que pdftotext pierde). Esto vale
    // incluso para PDFs escaneados, donde no hay texto extraíble.
    let images = render_pdf_images(path, MAX_PDF_IMAGES);
    if text.trim().is_empty() && images.is_empty() {
        return Err("Sin texto ni páginas renderizables para resumir (¿formato no soportado?).".into());
    }
    let muestra: String = text.chars().take(28000).collect();

    let instruccion = match category {
        "Ayudantía" | "Guía/Ejercicios" | "Laboratorio" => "\
Eres un ayudante experto. Estructura la respuesta en DOS PARTES BIEN SEPARADAS, en este orden:\n\n\
# Parte 1 — Enunciados\n\
Reproduce TAL CUAL (verbatim, copiado del documento, SIN resumir ni reformular) el enunciado de CADA ejercicio/problema, numerados en orden:\n\
### Ejercicio N\n\
(enunciado literal completo: todos los datos, incisos a/b/c, tablas y fórmulas que se den)\n\n\
# Parte 2 — Resolución paso a paso\n\
SOLO después de haber listado TODOS los enunciados, resuelve cada uno en orden:\n\
### Solución Ejercicio N\n\
**Desarrollo:** (cada paso justificado, con las fórmulas)\n\
**Resultado:** (respuesta final)\n\
Si el documento ya trae solución, explícala y completa los pasos que falten. No mezcles enunciados con soluciones: primero TODOS los enunciados, luego TODAS las soluciones.",
        "Prueba/Control" => "\
Resume esta evaluación para preparar el estudio:\n\
## Qué evalúa\n(temas y habilidades cubiertas)\n\
## Tipos de pregunta\n(viñetas)\n\
## Resolución\n(si incluye pauta/soluciones, resuelve los ítems PASO A PASO)\n\
## Para repasar\n(conceptos clave a dominar)",
        "Tarea" => "\
Extrae lo accionable de esta tarea:\n\
## Objetivo\n## Qué entregar\n(formato, archivos, requisitos)\n\
## Criterios de evaluación\n(si aparecen)\n## Plan sugerido\n(pasos para resolverla)",
        "Lectura/Paper" => "\
Resume esta lectura académica:\n\
## Idea central\n## Argumentos / desarrollo\n(viñetas)\n\
## Conceptos clave\n## Conclusión\n## Para repasar\n(2-3 preguntas)",
        // Clase, Cápsula y otros: resumen de estudio.
        _ => "\
Resume este material de clase para estudiar:\n\
## Resumen\n(2-4 frases con la idea central)\n\
## Puntos clave\n(conceptos importantes)\n\
## Definiciones / Fórmulas\n(si aplica)\n\
## Para repasar\n(2-3 preguntas de autoevaluación)",
    };

    // Bloque de visión: solo cuando adjuntamos imágenes de las páginas.
    let bloque_vision = if images.is_empty() {
        ""
    } else {
        "\nADJUNTO las páginas del documento como IMÁGENES. Léelas con atención: \
         contienen ejercicios, enunciados, figuras, diagramas, tablas y fórmulas que el \
         texto extraído NO incluye o transcribe mal. Cuando un ejercicio o una figura \
         aparezca solo en la imagen, TRANSCRÍBELO con fidelidad (incluidos los datos y \
         valores numéricos) y, si procede, RESUÉLVELO paso a paso. Prioriza lo que ves en \
         las imágenes frente al texto cuando haya discrepancia. Describe las figuras \
         relevantes en palabras.\n"
    };
    let contenido = if muestra.trim().is_empty() {
        "(sin texto extraíble; usa las imágenes adjuntas)".to_string()
    } else {
        muestra
    };
    let prompt = format!(
        "{instruccion}\n{bloque_vision}\n\
         Escribe en español, en markdown, claro y bien estructurado.\n\
         Documento ({category}): {filename}\n\n\
         Texto extraído (puede estar incompleto):\n{contenido}"
    );
    cloud_complete_mm(cfg, &prompt, &images).await
}

/// Resumen GENERAL de un ramo para una categoría, a partir de los resúmenes
/// individuales ya generados (compilado/síntesis del curso completo).
/// - `previous`: resumen general ANTERIOR (si existe) → se conserva e integra
///   con la información nueva (resumen incremental para ramos en curso).
/// - `web`: contexto de búsqueda web para COMPLEMENTAR/contrastar (puede ir vacío).
pub async fn summarize_course(cfg: &AppConfig, course_name: &str, category: &str, body: &str, previous: &str, web: &str) -> Result<String, String> {
    if body.trim().is_empty() {
        return Err("No hay resúmenes individuales en los que basarse.".into());
    }
    let muestra: String = body.chars().take(40000).collect();
    let instruccion = match category {
        "Ayudantía" | "Guía/Ejercicios" | "Laboratorio" => "\
Eres ayudante del ramo. A partir de los resúmenes de cada sesión, arma un COMPILADO GENERAL de ejercicios resueltos del curso, organizado POR TEMA (no por sesión):\n\
## Índice de temas\n## Por tema\n(para cada tema: tipos de ejercicio y el método de resolución paso a paso consolidado, con las fórmulas clave)\n## Errores comunes y trucos",
        "Clase" | "Cápsula" => "\
Eres profesor del ramo. A partir de los resúmenes de cada clase, redacta un RESUMEN GENERAL del curso para estudiar de principio a fin:\n\
## Hilo conductor\n(cómo se conectan las unidades)\n## Temario resumido\n(en orden, con los conceptos clave de cada unidad)\n## Definiciones y fórmulas esenciales\n## Mapa de estudio\n(qué dominar y en qué orden)",
        "Prueba/Control" => "\
Consolida todas las evaluaciones del ramo:\n## Qué entra\n(temas recurrentes)\n## Patrones de pregunta\n## Recomendaciones de estudio",
        "Lectura/Paper" => "\
Síntesis general de todas las lecturas del ramo:\n## Ideas transversales\n## Resumen por lectura\n## Conceptos clave",
        _ => "\
Crea una síntesis general del ramo a partir de los resúmenes dados, bien organizada en markdown.",
    };
    let bloque_previo = if previous.trim().is_empty() {
        String::new()
    } else {
        let prev: String = previous.chars().take(20000).collect();
        format!(
            "\n\nRESUMEN GENERAL ANTERIOR (este ramo aún está EN CURSO): consérvalo íntegro e \
             INTÉGRALO con la información nueva de abajo; NO pierdas nada del anterior, solo \
             amplíalo y reorganízalo con el material nuevo:\n{prev}\n"
        )
    };
    let bloque_web = if web.trim().is_empty() {
        String::new()
    } else {
        let w: String = web.chars().take(8000).collect();
        format!(
            "\n\nCONTEXTO WEB COMPLEMENTARIO (para enriquecer, contrastar y precisar definiciones; \
             úsalo con criterio, NO inventes ni te desvíes del temario del ramo):\n{w}\n"
        )
    };
    let prompt = format!(
        "{instruccion}\n\n\
         Escribe en español, en markdown claro. Ramo: {course_name} · Sección: {category}\
         {bloque_previo}{bloque_web}\n\n\
         Resúmenes individuales en los que basarte:\n{muestra}"
    );
    cloud_complete(cfg, &prompt).await
}

// ----------------------------------------------------------------------------
// Búsqueda web (sin clave) para complementar los resúmenes generales.
// ----------------------------------------------------------------------------

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn strip_tags(s: &str) -> String {
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
        .replace("&quot;", "\"").replace("&#x27;", "'").replace("&#39;", "'")
        .replace("&nbsp;", " ").trim().to_string()
}

/// Búsqueda web SIN clave (DuckDuckGo HTML). Devuelve título + extracto de los
/// primeros resultados, para COMPLEMENTAR resúmenes. Falla en silencio ("").
pub async fn web_search(query: &str, max_results: usize) -> String {
    let url = format!("https://html.duckduckgo.com/html/?q={}", url_encode(query));
    let client = reqwest::Client::new();
    let resp = match client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    let html = match resp.text().await { Ok(t) => t, Err(_) => return String::new() };

    // Títulos (result__a) y extractos (result__snippet) en orden de aparición.
    let titles: Vec<String> = html.split("result__a")
        .skip(1)
        .filter_map(|p| p.find('>').and_then(|g| p[g + 1..].find("</a>").map(|e| strip_tags(&p[g + 1..g + 1 + e]))))
        .collect();
    let snippets: Vec<String> = html.split("result__snippet")
        .skip(1)
        .filter_map(|p| p.find('>').and_then(|g| p[g + 1..].find("</a>").map(|e| strip_tags(&p[g + 1..g + 1 + e]))))
        .collect();

    let mut out = String::new();
    for i in 0..snippets.len().min(max_results) {
        let t = titles.get(i).map(|s| s.as_str()).unwrap_or("");
        let s = &snippets[i];
        if s.len() < 20 { continue; }
        if t.is_empty() {
            out.push_str(&format!("- {}\n", s));
        } else {
            out.push_str(&format!("- {}: {}\n", t, s));
        }
    }
    out
}

/// Genera el embedding de un texto con Ollama (nomic-embed-text).
pub async fn embed(cfg: &AppConfig, text: &str) -> Result<Vec<f32>, String> {
    let url = format!("{}/api/embeddings", cfg.ollama_url.trim_end_matches('/'));
    let muestra: String = text.chars().take(6000).collect();
    let body = serde_json::json!({ "model": cfg.embed_model, "prompt": muestra });
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Ollama embeddings: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Ollama HTTP {}", resp.status()));
    }
    let json: Value = resp.json().await.map_err(|e| e.to_string())?;
    let v = json
        .get("embedding")
        .and_then(|e| e.as_array())
        .ok_or("Respuesta de embedding vacía")?
        .iter()
        .filter_map(|x| x.as_f64().map(|f| f as f32))
        .collect();
    Ok(v)
}

/// Similitud coseno entre dos vectores.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Chat no-streaming contra Ollama (OpenAI-compat).
async fn ollama_chat(cfg: &AppConfig, model: &str, prompt: &str) -> Result<String, String> {
    let url = format!("{}/v1/chat/completions", cfg.ollama_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": false,
        "temperature": 0.2,
    });
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Ollama: {} (¿está corriendo `ollama serve`?)", e))?;
    if !resp.status().is_success() {
        return Err(format!("Ollama HTTP {}", resp.status()));
    }
    let json: Value = resp.json().await.map_err(|e| e.to_string())?;
    json.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|t| t.as_str())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "Respuesta vacía de Ollama".into())
}

/// Completion cloud para resúmenes (texto libre, temperatura media).
async fn cloud_complete(cfg: &AppConfig, prompt: &str) -> Result<String, String> {
    cloud_chat(cfg, prompt, 0.4, false).await
}

/// Completion cloud MULTIMODAL: el prompt va acompañado de imágenes (data-URIs)
/// de las páginas del PDF, para que la IA VEA ejercicios, figuras y fórmulas.
/// Si no hay imágenes, equivale a `cloud_complete` (solo texto).
async fn cloud_complete_mm(cfg: &AppConfig, prompt: &str, images: &[String]) -> Result<String, String> {
    if images.is_empty() {
        return cloud_complete(cfg, prompt).await;
    }
    let mut parts = vec![serde_json::json!({ "type": "text", "text": prompt })];
    for url in images {
        parts.push(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": url }
        }));
    }
    cloud_chat_content(cfg, serde_json::Value::Array(parts), 0.4, false).await
}

/// Chat cloud (OpenAI-compat) ROBUSTO: usa el proveedor + clave de Nexo, con
/// REINTENTOS y backoff ante errores transitorios (429 cuota, 503 saturación,
/// timeouts). `json_mode` fuerza salida JSON (para clasificación fiable).
async fn cloud_chat(cfg: &AppConfig, prompt: &str, temperature: f64, json_mode: bool) -> Result<String, String> {
    cloud_chat_content(cfg, serde_json::Value::String(prompt.to_string()), temperature, json_mode).await
}

/// Igual que `cloud_chat` pero el `content` del mensaje puede ser una cadena
/// (texto puro) o un array de partes OpenAI-compat (texto + imágenes), lo que
/// habilita las peticiones multimodales a Gemini.
async fn cloud_chat_content(cfg: &AppConfig, content: serde_json::Value, temperature: f64, json_mode: bool) -> Result<String, String> {
    let (openrouter, gemini) = config::nexo_keys();
    let (endpoint, key, extra_headers) = match cfg.summary_provider.as_str() {
        "gemini" => {
            if gemini.is_empty() {
                return Err("Falta la API key de Gemini en Nexo (~/.config/cortado/config.json).".into());
            }
            (
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions".to_string(),
                gemini,
                false,
            )
        }
        "openrouter" => {
            if openrouter.is_empty() {
                return Err("Falta la API key de OpenRouter en Nexo.".into());
            }
            (
                "https://openrouter.ai/api/v1/chat/completions".to_string(),
                openrouter,
                true,
            )
        }
        other => return Err(format!("Proveedor de resúmenes desconocido: {}", other)),
    };
    let mut body = serde_json::json!({
        "model": cfg.summary_model,
        "messages": [{"role": "user", "content": content}],
        "stream": false,
        "temperature": temperature,
    });
    if json_mode {
        body["response_format"] = serde_json::json!({ "type": "json_object" });
    }
    let client = reqwest::Client::new();

    // Hasta 4 intentos con backoff creciente para 429/503/timeout.
    let max_attempts = 4u32;
    let mut last_err = String::new();
    for attempt in 0..max_attempts {
        if attempt > 0 {
            // backoff: 2s, 5s, 12s
            let wait = match attempt { 1 => 2, 2 => 5, _ => 12 };
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        }
        let mut req = client.post(&endpoint).json(&body).bearer_auth(&key);
        if extra_headers {
            req = req
                .header("HTTP-Referer", "https://localhost/aula")
                .header("X-Title", "Aula UC");
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => { last_err = format!("Conexión cloud: {}", e); continue; }
        };
        let st = resp.status();
        if st.is_success() {
            let json: Value = match resp.json().await {
                Ok(j) => j,
                Err(e) => { last_err = e.to_string(); continue; }
            };
            let content = json.get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());
            match content {
                Some(s) if !s.trim().is_empty() => return Ok(s),
                _ => { last_err = "Respuesta cloud vacía".into(); continue; }
            }
        }
        let code = st.as_u16();
        let t = resp.text().await.unwrap_or_default();
        last_err = format!("HTTP {}: {}", st, t.chars().take(300).collect::<String>());
        // Solo reintenta errores transitorios; el resto falla de inmediato.
        if !(code == 429 || code == 500 || code == 502 || code == 503 || code == 504) {
            return Err(last_err);
        }
    }
    Err(format!("Agotados los reintentos cloud. Último error: {}", last_err))
}
