<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.80.9

**Motor de scraping y extracción web de alto rendimiento y memoria acotada para agentes de IA y pipelines de LLM.**  
Convierte cualquier sitio web, documento o archivo en Markdown limpio y datos estructurados: un único binario en Rust, SDK nativos, APIs duales Firecrawl v1/v2 y Jina Reader, con menos memoria que las alternativas medidas (véase la [sección 9 de benchmarks.md](benchmarks.md)).

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![200M tareas sintéticas](https://img.shields.io/badge/200M%20Synthetic-947%2C611%20p%2Fs-blue.svg)](releases/0.70.0.md)
[![768 MiB cgroup](https://img.shields.io/badge/768%20MiB%20cgroup-Zero%20OOM-success.svg)](releases/0.70.0.md)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/esA3wMZVvA)

[English](../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · **Español**

</div>

---

Medido el 2026-10-27 con el binario de la versión 0.80.0 frente a tres herramientas por dominio (un proyecto en Rust y dos muy comparados), con opciones por defecto, en una sola máquina (i7-10700, 15 GB). Las muestras se tomaron con semillas fijas tras congelar las reglas, sin elegir a mano. Cada fila enlaza al método, versiones, tamaño de muestra y resultados.

### Muy por delante

| Dominio | Métrica (dirección) | PageSift 0.80 | Competidor más fuerte | Evidencia |
| :--- | :--- | :--- | :--- | :--- |
| Office & e-books (docx, xlsx, pptx, epub, csv) | Structure kept, held-out ↑ | **0.988** | MarkItDown 0.717 | [§4](../benchmarks/mix/results/report-2026-10-27.md) |
| PDF, olmOCR-bench born-digital (1,269 pages) | Official score ↑ | **0.626** | pymupdf4llm 0.484 | [§3](../benchmarks/mix/results/report-2026-10-27.md) |
| Hard web pages (166 adversarial) | Completion ↑ / timeouts ↓ | **0.723** / 0 | crw 0.590 / 27 | [§1](../benchmarks/mix/results/report-2026-10-27.md) |
| Office & e-books speed (81 files, 1 worker) | Files/s ↑ | **57.4** | extractous 9.9 | [§4](../benchmarks/mix/results/report-2026-10-27.md) |

### Por delante

| Dominio | Métrica (dirección) | PageSift 0.80 | Competidor más fuerte | Evidencia |
| :--- | :--- | :--- | :--- | :--- |
| Main-content extraction, WCXB | F1 ↑ | **0.866** | Readability 0.848 | [§1](../benchmarks/mix/results/report-2026-10-27.md) |
| Main-content extraction, AEB | F1 ↑ | **0.956** | trafilatura 0.950 | [§1](../benchmarks/mix/results/report-2026-10-27.md) |
| Main-content extraction speed (269 pages, 16 workers) | Pages/s ↑ | **614** | crw 387 | [§1](../benchmarks/mix/results/report-2026-10-27.md) |
| PDF speed (144 files, 1 worker) | Files/s ↑ | **114** | pdf_oxide 94 | [§3](../benchmarks/mix/results/report-2026-10-27.md) |

### Por detrás pero cerca (en desarrollo activo)

| Dominio | Métrica (dirección) | PageSift 0.80 | Competidor más fuerte | Evidencia |
| :--- | :--- | :--- | :--- | :--- |
| PDF vs a layout model (144-page subset) | olmOCR score ↑ | 0.650 | Docling (CPU) 0.696 | [§3](../benchmarks/mix/results/report-2026-10-27.md) |
| Random real PDFs (71) | Text completeness ↑ | 0.936 | pdf_oxide 0.962 | [§3](../benchmarks/mix/results/report-2026-10-27.md) |
| Fetching, 269 random pages (local replay) | Completion ↑ | 0.937 | Scrapy 0.981 | [§2 — pagesift refuses captcha/block pages](../benchmarks/mix/results/report-2026-10-27.md) |
| Main-content extraction, Firecrawl set | Truth recall ↑ | 0.477 | crw 0.511 | [§1](../benchmarks/mix/results/report-2026-10-27.md) |
| Hard PDFs (100 pdf.js test files) | Completion ↑ | 0.560 | pymupdf4llm 0.680 | [§3 — the largest gap](../benchmarks/mix/results/report-2026-10-27.md) |

### Behind

| Domain | Metric (direction) | PageSift 0.80 | Strongest competitor | Evidence |
| :--- | :--- | :--- | :--- | :--- |
| Fetching speed (269 pages, 1 worker) | Pages/s ↑ | 2.7 | spider 153 | [§2 — default auto engine, p95 1.5 s](../benchmarks/mix/results/report-2026-10-27.md) |
| PDF speed (144 files, 16 workers) | Files/s ↑ | 142 | pdf_oxide 526 | [§3 — one process per file scales poorly](../benchmarks/mix/results/report-2026-10-27.md) |

Mientras tanto, los últimos resultados medidos (con dirección, versión y fecha) están en
[benchmarks.md](benchmarks.md); los scripts y resultados de cada dominio, en [benchmarks/](../benchmarks/).

---

## Escala: 200 millones de solicitudes en una sola máquina (0.70)

- **Prueba de saturación sintética de 200 millones de tareas en 192 vCPU**: 200.000.000/200.000.000 completadas, cero fallos, 947.611 páginas/s, 211,1 s y 888,1 MiB de RSS máximo. Esta cifra corresponde a carga sintética, no a rastreo de la web pública. Fuente: [`0.70_200m_regression.md`](history/0.70_200m_regression.md).
- **La memoria es una entrada**: el motor usa `memory.max` de cgroup v2 para ajustar la capacidad: con un límite de 768 MiB se midieron 380 conexiones, 817.346 páginas/s y ningún OOM. Fuente: [notas de la versión 0.70.0](releases/0.70.0.md).

Medido con 0.70.0; no se ha repetido con versiones posteriores.

---

## Novedades en 0.80.9

**La última versión de la serie 0.80.** El rastreo enfocado se comprobó en 69 universidades y cinco
direcciones (estudiantes internacionales, posgrado, alojamiento, informática, biblioteca): frente a
0.80.0, páginas justo sobre la consulta 2,6% → 19,0% y relacionadas 18,4% → 49,6%; los enlaces en cola
se puntúan de nuevo al aprender los pesos y una página con varias URL se entrega una vez. Los PDF
incluyen el texto de campos rellenados, notas, sellos y firmas (archivos difíciles de pdf.js
53 → 61 de 100). `scrape` arranca más ligero con un proceso por archivo (CPU por llamada 6,3 → 4,3 ms).
`doctor` arranca cada motor y avisa si alguno no funciona; se encuentran motores fuera del `PATH` y en
la caché de Playwright; `crawl`/`map` aceptan `--ignore-robots`. Los títulos de acordeones y FAQ se
conservan. Todas las pruebas quedan dentro del 2% de 0.80.0. No hubo 0.80.6–0.80.8. Detalles:
[notas de 0.80.9](releases/0.80.9.md).

## Novedades en 0.80.5

**Los resultados que parecen correctos pero no lo son ahora lo dicen.** Las advertencias que indican
un resultado incompleto o erróneo (páginas o contenido perdidos en un PDF, texto escaso o parcial,
salida truncada, un renderizado que solo trae el esqueleto…) llenan el `data.warning` de Firecrawl,
así que los clientes de Firecrawl las ven; `crawl --out` añade `summary.json`. Los controles de edad
dentro de un sitio grande (`/agecheck/` de Steam) y las páginas de bloqueo HTML etiquetadas como PDF
se informan como `ACCESS_BARRIER` en lugar de contenido. Word, Excel, PowerPoint, CSV y EPUB se
detienen en 16 MB de texto con `output_truncated` (un DOCX de 408 KB llegaba a 84 MB de Markdown).
Las advertencias de PDF dañados cuentan sobre las páginas reales del documento y nombran las páginas
perdidas a medias. El renderizado espera hasta 1,5 s más a las páginas que siguen vacías cuando la
red ya está en silencio. Los valores mal escritos de `--engine`, `--format` y `--sitemap` son
errores. Todas las pruebas de rendimiento quedan a menos del 2% de 0.80.0. No hubo 0.80.4. Detalle:
[notas de 0.80.5](releases/0.80.5.md).

## Novedades en 0.80.3

**Rastreo enfocado, comprobado en sitios universitarios reales.** Las mismas 38 portadas de
universidades se rastrearon con 0.80.0 y 0.80.3 (`crawl --query "international students admission
tuition scholarship visa" --adaptive --limit 12`). En los 32 sitios que ambas alcanzaron: páginas
útiles 29,6% → 66,1%, páginas para estudiantes internacionales 6,3% → 38,9%, mediana de páginas
útiles por sitio 1,5 → 8 (clasificación automática por URL y título; scripts y datos en
`benchmarks/history/0.80.3/`). Los rastreos con consulta cubren todo el sitio, devuelven al
presupuesto las descargas fallidas, encuentran el sitio en inglés por `hreflang` y subdominios en
inglés, y ya no tratan los menús como listas. Un PDF truncado que perdió el contenido de una página
dice cuál (`content_missing`). Detalle: [notas de 0.80.3](releases/0.80.3.md).

## Novedades en 0.80.2

**El rastreo enfocado encuentra las páginas que se le pidieron.** Un probador ejecutó `crawl --query`
sobre 159 sitios universitarios (*international students admission tuition scholarship visa*, 12 páginas
por sitio) y obtuvo un 25,6% de páginas útiles. Se reescribieron la puntuación de enlaces, la planificación
y la regla de parada: texto de ancla para todos los enlaces (navegación incluida), términos ponderados por
su rareza en el sitio, páginas de noticias y "acerca de" degradadas, secciones por turnos, subdominios
dentro del alcance, páginas de error excluidas y una parada adaptativa que solo cuenta páginas relevantes.
En los 8 sitios sintéticos que reproducen los patrones de fallo del informe, las páginas útiles pasaron del
7,3% al 82,6%; **los sitios reales no eran accesibles desde el entorno de desarrollo, así que esas cifras
son solo sintéticas.** Nuevo: `attachments` descarga los PDF y documentos Office enlazados desde páginas
relevantes; cada resultado lleva la traza del rastreo (`depth`, `parent`, `score`, `attachmentOf`).
PDF: un título bajo marcadores que solo cubren las secciones ya no queda vacío; un PDF linealizado cortado
antes de la raíz del árbol de páginas se convierte en vez de rechazarse. Detalle: [notas de 0.80.2](releases/0.80.2.md).

## Correcciones en 0.80.1

Correcciones de la revisión de 0.80 (la extracción de HTML no cambia; la salida de PDF solo cambia donde una marca de nota al pie elevada sigue a una palabra): los CSV en Shift_JIS, GBK o con
cualquier charset declarado se decodifican como el HTML (antes salían ilegibles y sin aviso); un PDF dañado que solo se
recupera en parte lo indica (`pdf_repaired`, `pages_missing`); las marcas de nota al pie tras los nombres de autores ya
no se escriben como fórmulas (`Ashish $Vaswani^{\ast}$` → `Ashish Vaswani*`); un PDF sin título incrustado recibe el
título inferido en `metadata.title` (`titleSource: inferred`); `*.localhost` resuelve a loopback en cualquier máquina,
así que las pruebas y las puertas de rendimiento funcionan en equipos solo IPv4; `NOTICE` (ahora con la licencia de
Adobe cmap-resources) acompaña a cada binario. Detalle: [notas de 0.80.1](releases/0.80.1.md).

## Novedades en 0.80.0

**PDF que conservan su estructura y resultados que avisan de lo que pueden haber perdido.**

- **Capa de archivos PDF propia** en lugar de lopdf: xref, flujos de objetos, filtros, cifrado y reparación de archivos dañados, más rápida y con menos memoria.
- **Títulos de PDF con niveles reales**, tomados primero del árbol de estructura y los marcadores del autor y, si no existen, deducidos de la numeración, el tamaño y la negrita. En los 13 PDF del informe AiSeek (más alto es mejor): exhaustividad 0,595 → 0,945, precisión 0,400 → 0,872, coincidencia de niveles 0,819 → 0,956, títulos vacíos 3 → 0.
- **Encabezados, pies y figuras**: se reconocen las marcas `/Artifact`, los encabezados con número de página y las figuras vectoriales. olmOCR-bench `headers_footers` 0,681 → 0,734 (más alto es mejor); tablas falsas en los 13 PDF 196 → 9 (más bajo es mejor).
- **CJK text without ToUnicode**: Chinese, Japanese and Korean PDFs whose CID fonts carry no
  `/ToUnicode` map (LaTeX/ctex with Fandol fonts, many others) lost all CJK text; it is now decoded
  through the Adobe-GB1/CNS1/Japan1/Korea1 collections or the embedded TrueType cmap. On 19 diverse
  Chinese PDFs, inferred-heading recall 0.497 → 0.826 (higher is better).
- **`POST /v2/parse`**: subida de archivos compatible con Firecrawl: PDF, HTML, Word (.docx), Excel (.xlsx), PowerPoint (.pptx), EPUB y CSV; `/v2/scrape` y `pagesift scrape <archivo>` convierten los mismos formatos.
- **Avisos `partial_content` y `replies_omitted`** cuando el texto extraído probablemente está incompleto. En 1.842 páginas guardadas de WCXB, pérdidas sin aviso 0,842 → 0,667 y falsas alarmas 0,174 → 0,130 (ambas, más bajo es mejor).
- **Correcciones**: en un lote, las páginas rápidas ya no esperan a las lentas del mismo sitio; el registro de auditoría de MCP identifica a los clientes del protocolo antiguo; `pagesift doctor` ya no escribe en el registro de auditoría.

Detalle, fuentes y resultados del conjunto reservado: [notas de 0.80.0](releases/0.80.0.md).

Versiones anteriores (0.73, 0.72.8, 0.71, 0.70, …): [CHANGELOG](../CHANGELOG.md) y las
[notas de versión](releases/). Las secciones retiradas de la portada (la tabla de siete dominios de 0.70,
la comparación con Spider y la matriz de 10 dominios de 0.50, la tabla con cifras de distintas versiones, etc.)
se conservan sin cambios y con sus fuentes en [history/homepage-es.md](history/homepage-es.md).

---

## Inicio Rápido

### 1. Docker / Servidor Autoalojado

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.80.9
```

#### Reemplazo Directo de Firecrawl SDK
Apunta tu aplicación Firecrawl existente a PageSift — sin cambios de código:

```python
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")
res = app.scrape_url("https://example.com", params={"formats": ["markdown"]})
print(res["markdown"])
```

#### Subir un archivo (`/v2/parse`)
Analiza un PDF, HTML, Word, Excel, PowerPoint, EPUB o CSV local o no público sin publicarlo en una URL: carga multipart compatible con Firecrawl, hasta 50 MB, misma respuesta que `/v2/scrape` ([detalles](parse-api.md)). La conversión de Office, EPUB y CSV es código Rust propio de pagesift (con sus propios lectores de ZIP y XML); en la línea de comandos, `pagesift scrape report.docx` hace lo mismo:

```bash
curl -s http://localhost:3002/v2/parse -H 'Authorization: Bearer sk-test' \
  -F file=@report.pdf -F 'options={"formats":["markdown"]}'
```

#### Reemplazo Directo de Jina Reader
Cualquier agente LLM o cliente HTTP puede invocar el endpoint `/r/`:

```bash
# Salida Markdown directa
curl -s http://localhost:3002/r/https://en.wikipedia.org/wiki/Rust_(programming_language)

# Salida SSE en tiempo real
curl -s -N -H "Accept: text/event-stream" http://localhost:3002/r/https://news.ycombinator.com
```

---

### 2. Paquete Python

```bash
pip install pagesift
```

```python
import pagesift

page = pagesift.scrape("https://example.com", formats=["markdown", "metadata"])
print(page.markdown)

results = pagesift.scrape_many(["https://site1.com", "https://site2.com"], concurrency=8)
```

---

### 3. Paquete Node.js / TypeScript

```bash
npm install @pagesift/node
# o
npm install pagesift
```

```typescript
import { scrape, scrapeMany } from '@pagesift/node'

const page = await scrape('https://example.com', {
  formats: ['markdown', 'links']
})
console.log(page.markdown)

const stream = scrapeMany(['https://a.com', 'https://b.com'], { concurrency: 4 })
for await (const page of stream) {
  console.log(`Scraped: ${page.url} in ${page.elapsedMs}ms`)
}
```

---

### 4. Interfaz de Línea de Comandos (CLI)

```bash
cargo install pagesift --locked

pagesift scrape "https://news.ycombinator.com" --format markdown

pagesift crawl "https://docs.rs" --limit 5000 --tuning-mode adaptive --out ./data/
```

### 5. Servidor MCP compartido para agentes de IA

```bash
# Un único servicio local para todos los agentes de la máquina (solo 127.0.0.1)
pagesift mcp http --port 3003 --audit-log ~/.local/state/pagesift/audit.jsonl
claude mcp add --scope user --transport http pagesift http://127.0.0.1:3003/mcp
```

Navegadores y caché compartidos, una línea de auditoría por descarga, instantáneas HTML de los
resultados sospechosos y la herramienta `pagesift_report_issue`. Véase
[Running pagesift as a shared MCP service](mcp-service.md).

---

## Comunidad

¿Dudas o comentarios? Únete a nuestro [Discord](https://discord.gg/esA3wMZVvA). La beta cerrada de la API alojada (próximamente, aún no disponible) también se anunciará allí.

---

## Licencia y Opciones Comerciales

- **Código Abierto:** Bajo licencia [AGPL-3.0](../LICENSE).
- **Licenciamiento Comercial:** ¿Requiere enlaces propietarios o despliegues privados sin obligaciones AGPL? Consulte nuestra [Guía de Licencias Comerciales](../COMMERCIAL.md) o contacte a `licensing@pagesift.dev`.
