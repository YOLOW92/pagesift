<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/docs/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.81.4

**Convierte páginas web, PDF y archivos de Office en Markdown limpio para LLM y agentes de IA.**  
Un único binario en Rust, autoalojado: CLI, API HTTP compatible con Firecrawl, endpoint de Jina Reader, servidor MCP, paquetes para Python y Node.js.

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![PageSift MCP server – quality and maintenance score on Glama](https://glama.ai/mcp/servers/YOLOW92/pagesift/badges/score.svg)](https://glama.ai/mcp/servers/YOLOW92/pagesift)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/esA3wMZVvA)

[English](../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · **Español**

</div>

<!-- readme-template: v1 -->
<!-- section: overview -->
## Qué es

pagesift descarga una URL (renderiza JavaScript solo cuando la página lo necesita) o lee un archivo local,
conserva el contenido principal y devuelve Markdown junto con metadatos. Extrae páginas sueltas, lotes y
sitios completos, y funciona como comando, biblioteca, servicio HTTP o servidor MCP.

Puede sustituir o acompañar a: **Firecrawl** (la misma API v1/v2, autoalojada), **Jina Reader** (`/r/<url>`),
**Crawl4AI**, **trafilatura** / **Readability**, **pymupdf4llm** / **pypdf**, **Kreuzberg**.

<!-- section: facts -->
## De un vistazo

- **Entrada:** URL (por HTTP, o con un navegador sin interfaz cuando hace falta), HTML, PDF, Word (.docx), Excel (.xlsx), PowerPoint (.pptx), EPUB, CSV
- **Salida:** Markdown, HTML, HTML sin procesar, enlaces, imágenes, metadatos (título, fecha de publicación, …), JSON; avisos cuando un resultado está incompleto
- **Interfaces:** CLI · API HTTP Firecrawl v1/v2 · Jina Reader `/r/` · MCP (stdio o HTTP) · Python · Node.js / TypeScript · crate de Rust
- **Plataformas:** Linux x86_64 / aarch64 (glibc y musl), macOS x86_64 / arm64, Windows x86_64; imagen Docker `ghcr.io/yolow92/pagesift`
- **Rastreo:** robots.txt y límites de tasa por host por defecto, sitemaps, rastreos enfocados por consulta, memoria acotada
- **Memoria:** pico de 14–380 MB por trabajador en las pruebas de 0.81 (el mínimo, con 16 trabajadores en HTML y PDF); la capacidad se ajusta al límite de memoria del cgroup (sin OOM con 768 MiB)
- **No sirve para:** OCR de PDF escaneados o imágenes; pagesift solo lee la capa de texto (ver *cuándo usarlo* más abajo)
- **Licencia:** AGPL-3.0, o una [licencia comercial](COMMERCIAL.md)
- **Última versión:** 0.81.4 · [notas de la versión](releases/0.81.4.md)

<!-- section: install -->
## Instalación y uso

| | Instalación | Primera llamada |
|---|---|---|
| CLI | `cargo install pagesift --locked` | `pagesift scrape https://example.com` |
| Python | `pip install pagesift` | `pagesift.scrape("https://example.com").markdown` |
| Node.js | `npm install @pagesift/node` | `(await scrape("https://example.com")).markdown` |
| Servidor | `docker run -p 3002:3002 ghcr.io/yolow92/pagesift:0.81.4` | `curl localhost:3002/r/https://example.com` |
| MCP | — | `claude mcp add pagesift -- pagesift mcp` |

**CLI**

```bash
pagesift scrape "https://news.ycombinator.com"              # Markdown a stdout
pagesift scrape report.pdf                                  # archivo local PDF, Office, EPUB, CSV o HTML
pagesift crawl "https://docs.rs" --limit 500 --out ./data/  # un archivo Markdown por página
pagesift crawl "https://www.example.edu" --query "graduate admission" --adaptive
pagesift map "https://docs.rs"                              # lista las URL de un sitio
pagesift doctor                                             # comprueba navegadores y entorno
```

**Python**

```python
import pagesift

page = pagesift.scrape("https://example.com", formats=["markdown", "metadata"])
print(page.markdown)
print(page.metadata.title, page.metadata.published_date)

results = pagesift.scrape_many(["https://site1.com", "https://site2.com"], concurrency=8)
```

**Node.js / TypeScript**

```typescript
import { scrape, scrapeMany } from '@pagesift/node'

const page = await scrape('https://example.com', { formats: ['markdown', 'links'] })
console.log(page.markdown)

for await (const p of scrapeMany(['https://a.com', 'https://b.com'], { concurrency: 4 })) {
  console.log(p.url, p.elapsedMs)
}
```

**Servidor HTTP (compatible con Firecrawl y Jina Reader)**

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.81.4
```

```python
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")  # código Firecrawl existente, URL nueva
print(app.scrape_url("https://example.com", params={"formats": ["markdown"]})["markdown"])
```

```bash
curl -s http://localhost:3002/r/https://example.com -H 'Authorization: Bearer sk-test'   # estilo Jina Reader
curl -s http://localhost:3002/v2/parse -H 'Authorization: Bearer sk-test' \
  -F file=@report.pdf -F 'options={"formats":["markdown"]}'                            # sube un archivo
```

Endpoints: `/v1` y `/v2` `scrape`, `batch/scrape`, `crawl`, `map`; `/v2/parse` (subida de archivos, hasta 50 MB,
[detalles](parse-api.md)); `/r/<url>` (Markdown plano o `text/event-stream`).

**Servidor MCP para agentes de IA**

```bash
pagesift mcp                                    # stdio, para un agente
pagesift mcp http --port 3003                   # un servicio local compartido (solo 127.0.0.1)
claude mcp add --scope user --transport http pagesift http://127.0.0.1:3003/mcp
```

Herramientas: `pagesift_scrape`, `pagesift_batch_scrape`, `pagesift_crawl`, `pagesift_crawl_status`, `pagesift_map`.
Navegadores y caché compartidos, un registro de auditoría y una herramienta para reportar problemas: [servicio MCP compartido](mcp-service.md).

<!-- section: choose -->
## Cuándo usarlo y cuándo no

**Encaja bien**

- Pasar páginas web y documentos a un LLM, un índice RAG o un agente, con Markdown que conserva encabezados, listas, tablas y enlaces
- Alojar en tus propias máquinas un servicio al estilo de Firecrawl o Jina Reader, sin tarifas por página
- Entrada mixta: la misma llamada maneja HTML, PDF, archivos de Office y libros electrónicos
- Máquinas pequeñas y contenedores: la memoria está acotada y se dimensiona a partir del límite del cgroup

**Elige otra cosa cuando**

- **Los PDF escaneados o las imágenes necesitan OCR.** pagesift solo lee la capa de texto; en 134 páginas escaneadas completó 0.112 frente a 1.000 de Docling en GPU. Usa Docling, olmOCR u otro pipeline de OCR.
- **Quieres el rastreo más rápido posible y no necesitas cortesía.** pagesift espera entre peticiones al mismo host, respeta robots.txt y reintenta los fallos; spider es mucho más rápido en tiempo bruto de rastreo (1.4 s frente a 16.6 s en 40 sitios × 60 páginas).
- **La AGPL no te sirve** y una licencia comercial no es una opción: usa una herramienta con licencia permisiva.
- **Necesitas hoy un servicio gestionado en la nube.** pagesift es autoalojado; todavía no hay una API alojada abierta.

<!-- section: performance -->
## Rendimiento

<!-- perf-glance -->
↑ más alto es mejor · ↓ más bajo es mejor. El resultado más fuerte por dominio; métodos y fuentes en el informe completo.

| Tarea | Métrica | PageSift | Mejor otra herramienta | Diferencia |
|---|---|---:|---|---:|
| PDF → Markdown — olmOCR-bench 1,269 pages, per page (single thread) | ms/page ↓ | **2.2** | pypdf 50.0 | **×22.32** |
| HTML → Markdown — 600 generated documents, per page | ms/page ↓ | **0.24** | Turndown 1.9 | **×8.24** |
| Main content — Firecrawl 597 pages, single thread | p50 ms/page ↓ | **1.0** | crw 7.4 | **×7.23** |
| Publication dates — htmldate 1,000 pages, date search | ms/page ↓ | **0.31** | htmldate (fast) 2.0 | **×6.45** |
| Office / e-books — B4 81 files, in-process | files/s ↑ | **94.1** | Kreuzberg 19.5 | **×4.82** |
| RAG chunking — 5 corpora, 1.4 MB | total ms ↓ | **11** | chonkie recursive 35 | **×3.24** |
| RAG query filter — 472 questions, default settings | answer recall ↑ | **0.929** | Crawl4AI BM25 0.673 | **+0.256** |
| Wild crawl — polite, 40 sites × 60 pages, coverage | coverage ↑ | **0.740** | Crawl4AI (HTTP) 0.529 | **+0.211** |

**Aún por detrás:** A2 replay, 269 pages, 1 worker 2.4 pages/s vs spider 156.6 pages/s; fast, 40 sites × 60 pages, time 16.6 seconds vs spider 1.4 seconds; B5 134 pages, completion 0.112 vs Docling (GPU) 1.000.

*El rendimiento de A2 se mide contra un servidor de reproducción en una sola dirección loopback, así que las 269 páginas cuentan como un mismo host. PageSift espera por defecto 1 s entre peticiones al mismo host; spider no tiene ese límite. Esta fila refleja sobre todo ese intervalo de cortesía, no la velocidad del motor.*

[Informe completo](../benchmarks/history/0.81/results/20261029-102426/report.md)

<!-- /perf-glance -->

Escala (0.70, origen local sintético, no velocidad en internet real): 200,000,000 de peticiones en 192 vCPU con 0 fallos a
947,611 páginas/s y 888 MiB de RSS máximo; dentro de un cgroup de 768 MiB mantuvo 817,346 páginas/s sin OOM.
Fuentes: [ejecución 0.70](history/0.70_200m_regression.md), [todos los benchmarks](benchmarks.md).

<!-- section: features -->
## Funciones

- **Extracción:** detección del contenido principal, Markdown que conserva en ida y vuelta encabezados, listas, tablas, código y fórmulas; fechas de publicación; filtros CSS `includeTags` / `excludeTags`; avisos como `partial_content`, `pages_missing`, `output_truncated` en lugar de resultados incompletos en silencio
- **Documentos:** lectores propios en Rust para PDF (árbol de estructura, marcadores, encabezados/pies de página, fuentes CJK sin ToUnicode, archivos dañados), Word, Excel, PowerPoint, EPUB y CSV
- **Renderizado:** `engine: auto` descarga primero por HTTP y abre un navegador sin interfaz solo cuando eso aporta contenido; la imagen Docker incluye uno
- **Rastreo:** rastreos enfocados (`--query … --adaptive`) que se detienen cuando las páginas dejan de aportar contenido relevante; adjuntos (PDF y archivos de Office enlazados); traza de rastreo por página; sitemaps; manejo de URL duplicadas y trampa; token buckets por host y espera progresiva ante 429/503
- **Operación:** colas acotadas, capacidad dimensionada según la CPU y la memoria del cgroup, persistencia de trabajos, webhooks, límites por clave y métricas en el servidor; registro de auditoría de cada descarga
- **Seguridad:** protecciones contra redes privadas y DNS rebinding por defecto (`--allow-cidr` para habilitarlas explícitamente), navegador en sandbox

Arquitectura: [architecture.md](architecture.md).

<!-- section: license -->
## Licencia

[AGPL-3.0](../LICENSE). Para enlazar con software propietario o desplegar en privado sin las obligaciones de la AGPL, consulta la
[licencia comercial](COMMERCIAL.md) o escribe a `licensing@pagesift.dev`.

<!-- section: changelog -->
## Novedades en 0.81.4

Actualización de seguridad de dependencias: tokio 1.54.1 y zstd 0.14.3 (zstd-safe 8.2.0) corrigen cuatro avisos de RustSec
(RUSTSEC-2026-0384, -0391, -0398, -0403). Sin cambios en la API.
[Notas de la versión](releases/0.81.4.md) · [todas las versiones](releases/) · [CHANGELOG](../CHANGELOG.md)

---

## Comunidad

Preguntas o comentarios: [Discord](https://discord.gg/esA3wMZVvA) · [Issues](https://github.com/YOLOW92/pagesift/issues) · [Discussions](https://github.com/YOLOW92/pagesift/discussions)

## Cita

```bibtex
@software{pagesift2026,
  title  = {PageSift: Rust Web and Document Extraction Engine for AI Pipelines},
  author = {YOLOW92},
  year   = {2026},
  url    = {https://github.com/YOLOW92/pagesift},
  note   = {Version 0.81.4}
}
```

## Historial de estrellas

[![Star History Chart](https://api.star-history.com/svg?repos=YOLOW92/pagesift&type=Date)](https://star-history.com/#YOLOW92/pagesift&Date)
