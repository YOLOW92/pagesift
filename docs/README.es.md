<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.80.0

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

## Cómo se compara PageSift

Agrupamos los resultados según la ventaja de PageSift. Las cifras finales de 0.80 se están repitiendo
con el binario de la versión frente a las herramientas más fuertes de cada dominio; las tablas quedan
vacías hasta que termine esa ejecución y no se escribe ninguna cifra sin medir. Cada fila enlazará al
método, los scripts y los resultados con fecha.

### Muy por delante

| Dominio | Métrica (dirección) | PageSift 0.80 | Competidor más fuerte | Evidencia |
| :--- | :--- | :--- | :--- | :--- |
| _pendiente de la ejecución final_ | | | | |

### Por delante

| Dominio | Métrica (dirección) | PageSift 0.80 | Competidor más fuerte | Evidencia |
| :--- | :--- | :--- | :--- | :--- |
| _pendiente de la ejecución final_ | | | | |

### Por detrás pero cerca (en desarrollo activo)

| Dominio | Métrica (dirección) | PageSift 0.80 | Competidor más fuerte | Evidencia |
| :--- | :--- | :--- | :--- | :--- |
| _pendiente de la ejecución final_ | | | | |

Mientras tanto, los últimos resultados medidos (con dirección, versión y fecha) están en
[benchmarks.md](benchmarks.md); los scripts y resultados de cada dominio, en [benchmarks/](../benchmarks/).

---

## Escala: 200 millones de solicitudes en una sola máquina (0.70)

- **Prueba de saturación sintética de 200 millones de tareas en 192 vCPU**: 200.000.000/200.000.000 completadas, cero fallos, 947.611 páginas/s, 211,1 s y 888,1 MiB de RSS máximo. Esta cifra corresponde a carga sintética, no a rastreo de la web pública. Fuente: [`0.70_200m_regression.md`](history/0.70_200m_regression.md).
- **La memoria es una entrada**: el motor usa `memory.max` de cgroup v2 para ajustar la capacidad: con un límite de 768 MiB se midieron 380 conexiones, 817.346 páginas/s y ningún OOM. Fuente: [notas de la versión 0.70.0](releases/0.70.0.md).

Medido con 0.70.0; no se ha repetido con versiones posteriores.

---

## Novedades en 0.80.0

**PDF que conservan su estructura y resultados que avisan de lo que pueden haber perdido.**

- **Capa de archivos PDF propia** en lugar de lopdf: xref, flujos de objetos, filtros, cifrado y reparación de archivos dañados, más rápida y con menos memoria.
- **Títulos de PDF con niveles reales**, tomados primero del árbol de estructura y los marcadores del autor y, si no existen, deducidos de la numeración, el tamaño y la negrita. En los 13 PDF del informe AiSeek (más alto es mejor): exhaustividad 0,595 → 0,945, precisión 0,400 → 0,872, coincidencia de niveles 0,819 → 0,956, títulos vacíos 3 → 0.
- **Encabezados, pies y figuras**: se reconocen las marcas `/Artifact`, los encabezados con número de página y las figuras vectoriales. olmOCR-bench `headers_footers` 0,681 → 0,734 (más alto es mejor); tablas falsas en los 13 PDF 196 → 18 (más bajo es mejor).
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
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.80.0
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
