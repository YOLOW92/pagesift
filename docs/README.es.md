<div align="center">

# pagesift 0.40.0

**Motor de scraping y extracción web de alto rendimiento y memoria acotada para agentes de IA y pipelines de LLM.**  
Convierte cualquier sitio web, documento o archivo en Markdown limpio y datos estructurados: un único binario en Rust, SDK nativos, APIs duales Firecrawl v1/v2 y Jina Reader, consumiendo una fracción de la memoria de la competencia.

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![Defensa 12GiB RAM](https://img.shields.io/badge/12GiB%20RAM%20Defense-Inmune%20a%20OOM-success.svg)](#-comparativa-de-rendimiento-los-8-dominios)

[English](../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · **Español**

</div>

---

## 🏆 Comparativa de Rendimiento (Los 8 Dominios)

Todas las pruebas se ejecutaron sobre hardware real **Intel Core i7-10700** (8 núcleos, 16 hilos, 15.5 GB RAM) bajo un **guardián de memoria de 12 GiB** activo (`MemoryMax=12288M`). Los conjuntos de datos CSV verificables están en [`bench/results/final/`](../bench/results/final/).

| Dominio | PageSift 0.40.0 | Principal Competidor | Ventaja / Aceleración | Memoria RAM Pico (PageSift vs Competidor) |
| :--- | :--- | :--- | :--- | :---: |
| **01. Extracción y Concurrencia** | **3,525 páginas/s** (0.57s) | `rs-trafilatura`: 241 p/s (8.32s)<br>`trafilatura` (Py): 191 p/s (10.51s) | **14.6× ~ 18.4× más rápido** | **137.8 MB** vs 298.3 MB |
| **02. Rastreo 50k y Defensa 12G** | **2,727 páginas/s** (17.76s)<br>**447.5 MB** pico RSS | `Spider 2.53.9`: 11.7 GB RSS<br>→ **ELIMINADO POR OOM (Límite 12GB)** | **Cero fugas de memoria**<br>(1/26 de la RAM) | **447.5 MB** vs 11,724.8 MB (💥 OOM) |
| **03. Extracción de PDF (olmOCR)** | **71.0%** aciertos, **44.3%** LaTeX<br>**3.4 ms/página** | `pymupdf4llm`: 64.4% aciertos, 0.0% LaTeX<br>320.0 ms/página | **94× más rápido**<br>Recuperación nativa de fórmulas TeX | **42.0 MB** vs 280.0 MB |
| **04. Autorreparación DOM** | **93.8%** aciertos, **0.0%** falsos pos<br>**24.6 µs** latencia | `Scrapling`: 46.0% aciertos, **51.2% falsos pos**<br>635.3 µs latencia | **25.8× más rápido**, cero engaños<br>DOM de 200 niveles seguro (928 µs) | **39.4 MB** vs 98.0 MB |
| **05. Particionado RAG y BM25** | R@1000 = **0.895** (19.7 ms)<br>Filtro BM25: **1.35 ms/pregunta** | `semchunk`: 113.4 ms (5.8× más lento)<br>`Crawl4AI`: 804.8 ms (**595× más lento**) | **5.8× más rápido en chunks**<br>**595× más rápido en BM25** | **45.2 MB** vs 340.0 MB |
| **06. Detección de Codificación** | **74.4%** precisión, **99.7%** Unicode<br>**0.45 ms/archivo** | `chardet 7.7`: 68.9% prec (3.05 ms)<br>`charset-normalizer`: 46.5% prec (1.05 ms) | **Mayor precisión (+5.5%)**<br>**6.8× más rápido** | **18.5 MB** vs 42.0 MB |
| **07. Datos Estructurados** | **80.2 µs/página** (estándar)<br>40 niveles de anidación: **363 µs** | `extruct 0.18`: 3,508 µs/página (43.8× más lento)<br>40 niveles de anidación: 15,921 µs | **43.8× ~ 43.9× más rápido**<br>Sin desbordamiento de pila | **24.0 MB** vs 85.0 MB |
| **08. Descubrimiento JS Implícito** | **13/13 (100.0%)** recuperación<br>**0 visitas no navegación**, **0.42 ms** | `Katana 1.8.0`: 10/13 (76.9%)<br>4 visitas no navegación (30.8% error), 48.5 ms | **100% recuperación**, cero trampas<br>Sin coste de sandbox de navegador | **14.2 MB** vs 128.0 MB |

> [!TIP]
> **Datos verificables por máquina**: Consulte el libro blanco completo en [`docs/releases/0.40.0.md`](releases/0.40.0.md) y los registros CSV en [`bench/results/final/`](../bench/results/final/).

---

## Novedades en 0.40.0

- 🌐 **Arquitectura Dual Firecrawl v1 y v2:** Compatibilidad total con los SDKs oficiales de Python y Node.js (`V1FirecrawlApp` y `FirecrawlApp`), cubriendo `/v1/scrape`, `/v1/crawl`, `/v1/batch/scrape`, `/v1/map` y todos los endpoints `/v2/*`.
- ⚡ **Adaptador de Protocolo Jina Reader:** Extracción directa mediante `GET /r/{*url}` o `POST /r`, con streaming mediante Server-Sent Events (`Accept: text/event-stream`) para alimentar agentes LLM en tiempo real.
- 🛡️ **Defensa de 12 GiB de RAM ante Caos:** En un rastreo real de 50,000 páginas, `Spider` colapsó por OOM (11.7 GB RSS), mientras PageSift completó 48,433 páginas a **2,727 páginas/s** con solo **447.5 MB** RSS. Superó un clúster de caos sintético de 100,000 URLs con DOMs de 150 niveles y 3,000 errores 503.
- 🦀 **100% Rust Seguro y Modularidad:** Mantenimiento de `#![forbid(unsafe_code)]` en `pagesift-api`. Todos los archivos fuente Rust cumplen estrictamente con `<= 500 líneas`.

---

## Inicio Rápido

### 1. Docker / Servidor Autoalojado

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.40.0
```

#### Reemplazo Directo de Firecrawl SDK
Basta con apuntar el SDK a PageSift, sin cambiar código:

```python
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")
res = app.scrape_url("https://example.com", params={"formats": ["markdown"]})
print(res["markdown"])
```

#### Reemplazo Directo de Jina Reader
Para agentes o peticiones curl directas:

```bash
# Markdown limpio directo
curl -s http://localhost:3002/r/https://es.wikipedia.org/wiki/Rust_(lenguaje_de_programaci%C3%B3n)

# Transmisión SSE en tiempo real
curl -s -N -H "Accept: text/event-stream" http://localhost:3002/r/https://news.ycombinator.com
```

---

### 2. Paquete Python

```bash
pip install pagesift
```

```python
import pagesift

# Extracción ultrarrápida (< 2ms)
page = pagesift.scrape("https://example.com", formats=["markdown", "metadata"])
print(page.markdown)
print(page.metadata.title, page.metadata.published_date)

# Rastreo concurrente con memoria acotada
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
  console.log(`Completado: ${page.url} en ${page.elapsedMs}ms`)
}
```

---

### 4. Interfaz de Línea de Comandos (CLI)

```bash
cargo install pagesift --locked

# Extraer una URL
pagesift scrape "https://news.ycombinator.com" --format markdown

# Rastreo educado de alta concurrencia
pagesift crawl "https://docs.rs" --limit 5000 --concurrency 16 --output ./data/
```

---

## Comparativa con la Competencia

| Característica | PageSift 0.40.0 | Firecrawl (Autoalojado) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **Arquitectura Base** | Rust 100% Seguro (`forbid(unsafe)`) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **Compatibilidad de APIs** | **Firecrawl v1+v2 + Jina + MCP** | Solo API de Firecrawl | API personalizada Python | API personalizada REST |
| **Rendimiento Extracción** | **3,525 páginas/s** | ~150 páginas/s | ~90 páginas/s | ~1,200 páginas/s |
| **RAM Pico (50k Páginas)** | **447.5 MB (Acotada)** | > 6 GB (Colas Redis) | > 8 GB (Pool navegador) | 💥 **Colapso OOM (11.7 GB)** |
| **Autorreparación DOM** | **93.8% (0.0% Falsos Pos)** | ❌ No | ❌ No | ❌ No |
| **PDF y Fórmulas LaTeX** | **71% Aciertos / 44.3% LaTeX** | Texto plano externo | Solo texto plano | Solo texto plano |
| **Particionado RAG y BM25** | **19.7 ms / 1.35 ms BM25** | ❌ No | Particionado Python (804 ms) | ❌ No |
| **Filosofía Antibot** | Respetuoso, tasa por host, UA real | Rotación de proxies | Evasión invisible | Evasión invisible |

---

## Licencia y Opciones Comerciales

- **Código Abierto:** Publicado bajo licencia [AGPL-3.0](LICENSE).
- **Licencia Comercial:** Si necesita integración privativa o despliegue privado sin las obligaciones de AGPL, consulte nuestra [Guía de Licencias Comerciales](../COMMERCIAL.md) o contacte a `licensing@pagesift.dev`.
