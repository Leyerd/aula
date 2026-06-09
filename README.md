# Aula — Gestor Universitario UC

App de escritorio (Tauri 2 + Rust + TypeScript, tema **"Midnight Academia"** — tinta + oro viejo + serif editorial) para **Canvas de la Pontificia Universidad Católica de Chile**. Reúne en un solo lugar:

- **Tareas y evaluaciones pendientes** de todos los ramos, ordenadas por fecha de entrega (atrasadas / esta semana / próximas). Puedes **marcarlas como hechas, descartarlas o restaurarlas** a mano; el override se conserva al re-sincronizar.
- **Descarga automática** de todos los archivos de todos los ramos en `/mnt/linux/Universidad/<Ramo>/<Subcarpeta>/…`. Los **cursos pasados** también se descargan, en `Cursos pasados/<Ramo>/…`, y siempre con **menor prioridad** (al final, tanto al descargar como al resumir).
- **Clasificación con IA local** (Ollama) según la nomenclatura UC: _Clase · Ayudantía · Cápsula · Guía/Ejercicios · Tarea · Prueba/Control · Lectura/Paper · Programa · Administrativo_.
- **Índice semántico local** (embeddings `nomic-embed-text`) para **buscar por significado**.
- **Resúmenes a medida por tipo**, con la **mejor IA cloud** (Gemini / OpenRouter, claves de **Nexo**): las **clases** se resumen para estudiar; las **ayudantías y guías** se compilan **resueltas paso a paso**; las **pruebas** listan temas y resuelven la pauta; las **tareas** extraen objetivo/entregables. Se guardan separados por tipo en `<Ramo>/_Resúmenes/<Tipo>/`.
- **Aviso de caducidad del token** (los de Canvas UC duran 120 días): banner cuando quedan ≤15 días y mensaje claro si caduca (HTTP 401).

## Cómo conseguir el token de Canvas (1 vez)

1. Entra a `https://cursos.canvas.uc.cl`.
2. **Cuenta** (avatar) → **Configuración**.
3. Baja hasta **Tokens de acceso aprobados** → **+ Nuevo token de acceso**.
4. Ponle un propósito (ej. "Aula") y déjalo sin fecha de caducidad.
5. Copia el token y pégalo en **Aula → Ajustes → Token de acceso**.
6. Pulsa **Probar conexión**: debe saludarte con tu nombre.

> El token se guarda en `~/.config/aula/config.json` con permisos `600` (solo tú).

## Flujo de uso

1. **Sincronizar** → trae ramos, tareas y el listado de archivos.
2. **Descargar todo** → baja los archivos a `/mnt/linux/Universidad/` (configurable). Cursos pasados al final.
3. **Clasificar** → la IA local etiqueta cada archivo y genera el índice de búsqueda.
4. **Resumir** → la IA cloud resume apuntes, lecturas, slides y guías.

Todo es incremental: re-sincronizar no vuelve a descargar ni reprocesar lo ya hecho.

## Integración con Nexo

Las claves cloud (OpenRouter / Gemini) se leen de tu app **Nexo** en
`~/.config/cortado/config.json`. No hay que duplicarlas. En **Ajustes** eliges
proveedor y modelo de resúmenes (por defecto `gemini-3-flash-preview`).

## IA local (Ollama)

Requiere `ollama serve` activo. Modelos usados:
- `qwen2.5:7b` — clasificación de documentos.
- `nomic-embed-text` — embeddings para búsqueda semántica.

## Desarrollo

```bash
npm install
npm run tauri dev      # desarrollo (hot reload)
npm run tauri build    # binario release + .deb/.rpm
```

> En NVIDIA + Wayland el lanzador `~/.local/bin/aula` ya exporta
> `WEBKIT_DISABLE_DMABUF_RENDERER=1`.

## Lanzar

- CLI: `aula`
- Lanzador de apps: **"Aula"**

## Arquitectura

- `src-tauri/src/config.rs` — config propia + import de claves de Nexo.
- `src-tauri/src/canvas.rs` — cliente REST de Canvas (paginación, ramos, tareas, archivos con _fallback_ a módulos, descarga).
- `src-tauri/src/ai.rs` — extracción de texto (PDF/txt), clasificación + embeddings (Ollama) y resúmenes (cloud OpenAI-compat).
- `src-tauri/src/store.rs` — estado persistente en `<descargas>/.aula/state.json`.
- `src-tauri/src/lib.rs` — comandos Tauri y orquestación con eventos de progreso.
- `src/main.ts` + `styles.css` — UI (vanilla TS, tema "Midnight Academia").

## Límites conocidos

- Resúmenes y clasificación leen texto de **PDF y texto plano**. Para `.docx`/`.pptx`
  o PDFs escaneados (imágenes) no se extrae texto todavía (quedan sin resumen).
- La descarga es secuencial (prioriza estabilidad y progreso visible).
