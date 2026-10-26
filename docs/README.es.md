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

## Novedades en 0.80.0

**PDF que conservan su estructura y resultados que avisan de lo que pueden haber perdido.**

- **Capa de archivos PDF propia** en lugar de lopdf: xref, flujos de objetos, filtros, cifrado y reparación de archivos dañados, más rápida y con menos memoria.
- **Títulos de PDF con niveles reales**, tomados primero del árbol de estructura y los marcadores del autor y, si no existen, deducidos de la numeración, el tamaño y la negrita. En los 13 PDF del informe AiSeek (más alto es mejor): exhaustividad 0,595 → 0,945, precisión 0,400 → 0,872, coincidencia de niveles 0,819 → 0,956, títulos vacíos 3 → 0.
- **Encabezados, pies y figuras**: se reconocen las marcas `/Artifact`, los encabezados con número de página y las figuras vectoriales. olmOCR-bench `headers_footers` 0,681 → 0,734 (más alto es mejor); tablas falsas en los 13 PDF 196 → 18 (más bajo es mejor).
- **`POST /v2/parse`**: subida de archivos PDF y HTML compatible con Firecrawl.
- **Avisos `partial_content` y `replies_omitted`** cuando el texto extraído probablemente está incompleto. En 1.842 páginas guardadas de WCXB, pérdidas sin aviso 0,842 → 0,667 y falsas alarmas 0,174 → 0,130 (ambas, más bajo es mejor).
- **Correcciones**: en un lote, las páginas rápidas ya no esperan a las lentas del mismo sitio; el registro de auditoría de MCP identifica a los clientes del protocolo antiguo; `pagesift doctor` ya no escribe en el registro de auditoría.

Detalle, fuentes y resultados del conjunto reservado: [notas de 0.80.0](releases/0.80.0.md).

## Novedades en 0.73.0

0.73.0 corrige lo que encontraron una revisión de código en tres frentes y unas 150 pruebas con entradas adversas.

- **La extracción vuelve a usar todos los núcleos**: desde 0.70.0, `scrape`, `crawl` y `serve` extraían en un solo núcleo; el servidor API atiende unas 3,5–7× más peticiones por segundo (16 clientes, motor `auto`, ejecuciones alternas en un i7-10700: 309–336 → 1.153–2.196 req/s; p50 de 36–45 ms a 4,5–6,1 ms).
- **Sin los fallos ni vías de DoS encontrados**: pánico en páginas CJK, páginas UTF-8 convertidas en mojibake, análisis cuadrático de atributos (40.000 atributos: 3 s → 0,02 s), clientes slowloris y enlaces `javascript:` en elementos RSS.
- **Heurísticas acotadas**: los artículos cortos de Substack se marcaban como verificación de edad; widgets de autores, raíces de aplicación con cloak, contenido de `<template>` y variables CSS tomadas por claves de OpenAI se tratan correctamente.
- **Límites y tiempos de espera en la API** para cada parámetro, con el cuerpo de error JSON estándar en todas partes.
- **Alcance de crawl decidido por evidencias** (`--scope auto`, por defecto): `/docs/python` se queda en `/docs/python/*`; una semilla hoja, como un artículo de blog, se amplía a su sección y lo avisa.

La calidad se mantiene o mejora (WCXB F1 0,8522 → 0,8533; Firecrawl 0,680 → 0,682; AEB 0,958) y la extracción es un 3–5% más rápida en páginas reales. Detalle: [notas de 0.73.0](releases/0.73.0.md).

## Novedades en 0.72.8

0.72.8 cierra la cuarta auditoría y las rondas 5–8 de campo, e incorpora el descubrimiento de configuración por defecto.

- **Verificación del charset**: si `Content-Type` o `<meta>` mienten, el cuerpo se vuelve a decodificar y la corrección aparece en `metadata.pagesift.warnings`.
- **`--allow-cidr` en todos los comandos**: `crawl`, `map`, `download`, `session-fetch`, `collect` y `serve` comparten la lista de `scrape`; un CIDR inválido falla en el parser.
- **Configuración por defecto**: `--config` > `PAGESIFT_CONFIG` > archivo de plataforma; `PAGESIFT_CONFIG=` lo desactiva. `doctor` muestra el archivo cargado.
- **Sin falsos éxitos**: `map` devuelve el error del seed y `crawl` sale con código 1 si no obtuvo páginas y hubo errores.
- **El cuerpo no se pierde**: se conservan los `<aside>` de contenido (listas de ingredientes) y las vistas previas `quickview`/`teaser` ya no sustituyen al artículo.
- **Páginas de código de GitHub**: se descarta la columna de números y las líneas se agrupan en un bloque de código.
- **Autores y metadatos**: la línea de autores de arXiv se conserva y `citation_author` rellena `metadata.author`; `jsonLd` se expone tal cual.
- **Señales honestas**: bloqueos en alemán/francés/español son `ACCESS_BARRIER`, las puertas de edad `interstitial`, la extracción escasa `thin_content` y un resultado que probablemente solo tiene parte del texto principal lo indica con `partial_content` o `replies_omitted`.
- **Feeds y medios**: RSS/Atom se convierten en listas de entradas; las imágenes conservan `title` y los vídeos usan `VideoObject` para título y descripción.

Detalle: [notas de 0.72.8](releases/0.72.8.md).

---

## Novedades en 0.70.0

La versión 0.70 reconstruye el flujo de datos HTTP con ejecutores locales a NUMA, un reactor HTTP/1.1 ligero, conexiones adaptativas por origen y procesamiento fragmentado con límites. El motor usa `memory.max` de cgroup v2 para ajustar la capacidad: con un límite de 768 MiB se midieron 380 conexiones, 817.346 páginas/s y ningún OOM.

- **Prueba de saturación sintética de 200 millones de tareas en 192 vCPU**: 200.000.000/200.000.000 completadas, cero fallos, 947.611 páginas/s, 211,1 s y 888,1 MiB de RSS máximo. Esta cifra corresponde a carga sintética, no a rastreo de la web pública.
- **Resultados públicos que incluyen las derrotas**: PageSift lidera 4 de 7 dominios; las diferencias en extracción de contenido, renderizado dinámico y autorreparación DOM figuran abajo.
- **Correcciones operativas**: `timeout_secs` limita también la espera de cabeceras; se recuperan las penalizaciones 429; una URL fallida ya no bloquea todo el host; se evita el panic con un `ulimit -v` bajo.

El diseño, las condiciones de medición y la migración están en las [notas de la versión 0.70.0](releases/0.70.0.md).

---

## 🏆 Comparación pública en siete dominios (0.70, medida en octubre de 2026; no repetida en versiones posteriores)

Todas las cifras proceden de **ejecuciones completas en la misma máquina**, con el evaluador propio de cada conjunto y un límite de memoria de 10 GiB. Equipo: Intel i7-10700 (8C/16T), 15 GB de RAM y Ubuntu 26.04. Los scripts están en [`bench/scripts/`](../bench/scripts/) y los informes por dominio en [`docs/benchmarks/`](benchmarks/).

| Dominio | PageSift 0.70 | Mejor rival | Puesto | RAM máxima |
| :--- | :--- | :--- | :---: | ---: |
| D1 Extracción de contenido (WCXB, 2.006 páginas) | 0,6652 F1 | PageSift 0.50: 0,6824 | 4.º | 751 MiB |
| D3 PDF (olmOCR, 1.403 documentos) | **0,4506** | pymupdf4llm: 0,2446 | **1.º** | 271 MiB |
| D5 Fragmentación RAG y BM25 (472 preguntas) | **0,9470 R@5** (su mejor estrategia; `recursive` obtiene 0,9004) | semchunk: 0,9364 | **1.º** | — |
| D6 Detección de codificación (3.128 archivos) | **0,7602** | chardet: 0,7113 | **1.º** | 39 MiB |
| D7 Datos estructurados (2.006 páginas) | **0,7392** | bs4+lxml: 0,5488 | **1.º** | **21 MiB** |
| D9 Renderizado dinámico (7 escenarios SPA) | 3/7 | Playwright: 5/7 | 2.º | — |
| D4 Autorreparación DOM (2.000 mutaciones) | 0,8215 | Scrapling: 0,8875 | 2.º | — |

En D1 se prioriza la precisión (0,8938, segunda de 17; la primera es goose3 con 0,9097) a costa de la cobertura (0,5851), y 0.70 queda 0,017 F1 por debajo de 0.50; en D9 la cifra de Playwright (≈ 2,55 s) incluye una espera fija de 2 s añadida por el script de prueba, así que no es una comparación de velocidad en igualdad de condiciones, y tampoco esperamos contenido insertado por `setTimeout`; D4 gana frente a señuelos (100 % frente a 86,4 %), pero queda segundo en conjunto (alrededor del 14,6 % de los casos de «precio» de ese conjunto tienen la etiqueta mal, según nuestra propia revisión). Las mediciones históricas que siguen pertenecen a versiones anteriores.

---

## ⚔️ Datos históricos (0.50): 100,000 dominios frente a Spider

**Entorno de Hardware**: **Servidor Cloud de Producción AMD EPYC 9B45 (32 Núcleos Físicos, 128 GB RAM, Red Pública >1 Gbps)**  
**Conjunto de Evaluación**: Top 100,000 Dominios Globales Oficiales de Tranco bajo condiciones idénticas (768 hilos concurrentes, 3.5s conexión / 10s tiempo total, deduplicación Blake3, extracción a Markdown en memoria pura):

```text
+-----------------------------------------------------------------------------------------+
| PageSift 0.50 vs Spider 2.53.9 (100,000 Dominios en Servidor de 32 Núcleos y 128GB RAM)|
+-----------------------------------------------------------------------------------------+
| Dimensión de Métrica         | PageSift 0.50 (Rust)    | Spider 2.53.9 (Rust)   | Veredicto
+------------------------------+-------------------------+------------------------+-------+
| Sitios Reales Procesados     | 105,660 URLs            | 100,000 URLs           | Empate
| Páginas Crudas 200 OK        | 62,937 páginas          | 72,209 (con errores)   | Spider
| Markdown Válido (>100B)      | 60,659 documentos ricos | 59,803 documentos      | PageSift (+856 docs)
| Páginas con ≥100B de Markdown| 96.38%                  | 82.82%                 | PageSift
| Memoria RAM Pico (RSS)       | 188.4 MB                | 8,978.9 MB             | PageSift (47.6× menor)
| Escala de millones           | No medido (extrapolación)| No medido (extrapolación)| —

+-----------------------------------------------------------------------------------------+
```

> **Notas (0.50, una sola ejecución; datos históricos, no repetidos con la versión actual)**
> - **Memoria**: el pico de PageSift fue de **188.4 MB** y el de Spider de **8,978.9 MB** (alcanzado en unos 2,5 minutos). Spider también completó las 100,000 URL: «eliminado por OOM» o «cuelgue a 14 GB» no son resultados de esta ejecución (el comportamiento a 1 millón es una extrapolación, no una medición).
> - **Velocidad**: Spider es más rápido (666.6 QPS sostenidos frente a 93.5; 72,209 respuestas 200 OK frente a 62,937).
> - **Extracción**: la «tasa de conversión» es la proporción de páginas 200 OK para las que el extractor de cada herramienta devolvió ≥ 100 bytes de Markdown (PageSift 96.38 %, Spider 82.82 %); no mide fidelidad. Documentos válidos: 60,659 frente a 59,803.
> - Informe: [`bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md`](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md).

---

## 🏆 Matriz de los 10 dominios (datos históricos de 0.50, no repetidos con la versión actual)

Evaluado sobre servidor **AMD EPYC de Producción (32 Núcleos, 128 GB RAM)** y verificado en máquina de escritorio **Intel Core i7-10700 (8C/16T, 15.5 GB RAM)** con un **guardián de memoria de 12 GiB** activo (`MemoryMax=12288M`). Datos CSV verificables archivados en [`bench/results/final/`](https://github.com/YOLOW92/pagesift/tree/main/bench/results/final/).

| Dominio | PageSift 0.50.0 | Principal Competidor | Ventaja / Aceleración | Memoria RAM Pico (PageSift vs Competidor) | Informe Verificado |
| :--- | :--- | :--- | :--- | :---: | :--- |
| **01. Extracción y Concurrencia** | **3,525 páginas/s** (0.57s)¹ | `rs-trafilatura`: 241 p/s (8.32s)<br>`trafilatura` (Py): 191 p/s (10.51s) | **14.6× ~ 18.4× más rápido** | **137.8 MB** vs 298.3 MB | [D1 Informe](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D1_CONTENT_EXTRACTION_050.md) |
| **02. Rastreo 100k y Gran Defensa**| **105,660 sitios (188.4 MB)**<br>Chaos: **2,727 p/s** (447.5 MB) | `Spider 2.53.9`: 8,978 MB (100k, completó)² | Unas 47.6× menos memoria (Spider es más rápido) | **188.4 MB** vs 8,978.9 MB | [100k Informe](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md) |
| **03. Extracción de PDF (olmOCR)** | **71.0%** aciertos, **44.3%** LaTeX<br>**3.4 ms/página** (subconjunto de 1,438 páginas)³ | `pymupdf4llm`: 64.4% aciertos, 0.0% LaTeX<br>320.0 ms/página | **94× más rápido**<br>Recuperación nativa de fórmulas TeX | **42.0 MB** vs 280.0 MB | [D3 Informe](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D3_HEAVY_PDF_EXTRACTION_050.md) |
| **04. Autorreparación DOM** | **93.8%** aciertos, **0.0%** falsos pos<br>**24.6 µs** latencia (500 casos)⁴ | `Scrapling`: 46.0% aciertos, **51.2% falsos pos**<br>635.3 µs latencia | **25.8× más rápido**, cero engaños<br>DOM de 200 niveles seguro (928 µs) | **39.4 MB** vs 98.0 MB | [D4 Informe](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D4_DOM_SELF_HEALING_050.md) |
| **05. Particionado RAG y BM25** | R@1000 = **0.895** (19.7 ms)<br>Filtro BM25: **1.35 ms/pregunta** | `semchunk`: 113.4 ms (5.8× más lento)<br>`Crawl4AI`: 804.8 ms (**595× más lento**) | **5.8× más rápido en chunks**<br>**595× más rápido en BM25** | **45.2 MB** vs 340.0 MB | [D5 Informe](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D5_RAG_CHUNKING_050.md) |
| **06. Detección de Codificación** | **74.4%** precisión, **99.7%** Unicode<br>**0.45 ms/archivo**⁵ | `chardet 7.7`: 68.9% prec (3.05 ms)<br>`charset-normalizer`: 46.5% prec (1.05 ms) | **Mayor precisión (+5.5%)**<br>**6.8× más rápido**⁵ | **18.5 MB** vs 42.0 MB | [D6 Informe](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D6_CHARSET_DETECTION_050.md) |
| **07. Datos Estructurados** | **80.2 µs/página** (estándar)<br>40 niveles de anidación: **363 µs** | `extruct 0.18`: 3,508 µs/página (43.8× más lento)<br>40 niveles de anidación: 15,921 µs | **43.8× ~ 43.9× más rápido**<br>Sin desbordamiento de pila | **24.0 MB** vs 85.0 MB | [D7 Informe](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D7_STRUCTURED_DATA_050.md) |
| **08. Descubrimiento JS Implícito** | **13/13 (100.0%)** (solo 13 endpoints)⁶<br>**0 visitas no navegación**, **0.42 ms** | `Katana 1.8.0`: 10/13 (76.9%)<br>4 visitas no navegación (30.8% error), 48.5 ms | **100% recuperación**, cero trampas<br>Sin coste de sandbox de navegador | **14.2 MB** vs 128.0 MB | [D8 Informe](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D8_JS_ENDPOINTS_050.md) |
| **09. Renderizado Headless (CDP)**| **158.4 ms/render** (pool caliente)<br>**100%** pases de canvas y SVG⁷ | `Lightpanda`: 82 ms (fallo en scripts SPA)<br>`Playwright`: 842 ms (5.3× más lento) | **5.3× más rápido que Playwright**<br>100% fidelidad de renderizado DOM | **188.0 MB** vs 1,840.0 MB | [D9 Informe](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D9_DYNAMIC_HEADLESS_050.md) |
| **10. Gateways de API de IA** | **308 req/s** (`/v2/scrape`, 1,000 peticiones), p50 4.37 ms⁸ | Gateway de Crawl4AI: 33.7 req/s, p50 451 ms (50 peticiones)⁸ | ≈ 9× (muestra pequeña)⁸ | **38.0 MB** vs 420.0 MB | [D10 Informe](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D10_AI_API_GATEWAYS_050.md) |

**Notas por fila (revisión de octubre de 2026)**
¹ El texto del informe D1 da 6,273.9 páginas/s para 0.50 (16 hilos), que no coincide con el CSV (3,525); ninguna de las dos cifras se ha repetido.
² En el informe de 100k Spider terminó (pico 8,978.9 MB; 666.6 QPS frente a 93.5 de PageSift); el «OOM» corresponde a pruebas anteriores de 0.40 con límite de 12 GB.
³ Subconjunto de 1,438 páginas; con olmOCR-bench completo (1,403 PDF / 7,019 comprobaciones) en la misma máquina con 0.70: 0,4506 frente a 0,2446 de pymupdf4llm; fórmulas 0,3530 ([D3](benchmarks/0.70_showdown_D3.md)).
⁴ 500 casos; con 2,000 casos en la misma máquina (0.70): PageSift 0,8215, Scrapling 0,8875; 0.50 obtuvo 0,6965 ([D4](benchmarks/0.70_showdown_D4.md)).
⁵ Sustituido por la medición de 0.70: 0,7602 frente a chardet 0,7113; velocidad 11,239 frente a 6,800 archivos/s, unas 1,65× y no 6,8× ([D6](benchmarks/0.70_showdown_D6.md)).
⁶ Solo 13 endpoints; el mismo CSV incluye jsluice con 11/13 (84,6 %), que esta tabla no muestra.
⁷ 0.70 en la misma máquina, 7 escenarios SPA: PageSift 3/7, Playwright 5/7 ([D9](benchmarks/0.70_showdown_D9.md)).
⁸ La fila original del CSV («Portkey / LiteLLM, 2,450 req/s») no coincide con el texto del informe y ambos son gateways de LLM, no scrapers; no se puede rastrear, así que se eliminó el CSV y se usan las cifras del informe.

> [!TIP]
> **Datos verificables por máquina**: Consulte el libro blanco completo en [`docs/releases/0.50.0.md`](releases/0.50.0.md) y los registros CSV en [`bench/results/final/`](https://github.com/YOLOW92/pagesift/tree/main/bench/results/final/).

---

## Novedades en 0.50.0

- 🧠 **Motor Dinámico Adaptativo de Concurrencia y Tiempos de Espera (`pagesift::adaptive`)**:
  - **Modelo Restringido por Cuello de Botella (Principio $\min$)**: La concurrencia se acota rigurosamente según la intersección de núcleos CPU, memoria física libre (2MB/conexión) y límite de descriptores de archivos del sistema operativo (`ulimit -n`):
    $$\text{Concurrencia} = \min\big(C_{cpu},\; M_{mem},\; FD_{safe}\big)$$
  - **Perfiles Automáticos de Hardware**: Micro (1C1GB), Small (2C2GB~4C4GB), Mid Desktop (8C16GB / i7-10700), High Server (32C128GB / AMD EPYC) y Extreme Supernode (64C+ 128GB+ FD>=65k).
  - **Modos Duales de Operación**: `--tuning-mode adaptive` (saturación óptima y segura del hardware) frente a `--tuning-mode conservative` (mantiene CPU < 20% para estaciones de trabajo).
  - **Tiempos de Espera RTT Seguros**: Conexión base ajustada a 3.5s y tiempo total a 10.0s para preservar conexiones internacionales legítimas con latencias elevadas.
- 🌐 **Arquitectura Dual Firecrawl v1 y v2:** Compatibilidad total con los SDKs oficiales de Python y Node.js, cubriendo `/v1/scrape`, `/v1/crawl`, `/v1/batch/scrape`, `/v1/map` y todos los endpoints `/v2/*`.
- ⚡ **Adaptador de Protocolo Jina Reader:** Extracción directa mediante `GET /r/{*url}` o `POST /r`, con streaming mediante Server-Sent Events (`Accept: text/event-stream`) para alimentar agentes LLM en tiempo real.
- 🛡️ **Servidor de 128 GB y Cero Fugas**: Memoria fija de **188.4 MB** a lo largo de 100,000 sitios web reales, en la misma ejecución Spider llegó a 8,978.9 MB (y fue más rápido).
- 🦀 **Casi todo Rust seguro:** todos los crates llevan `#![forbid(unsafe_code)]` salvo `pagesift-sys` (una llamada `mallopt` de glibc). (Se retiró la frase «todos los archivos ≤ 500 líneas»: ya no es cierta.)

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
Analiza un PDF o HTML local o no público sin publicarlo en una URL: carga multipart compatible con Firecrawl, hasta 50 MB, misma respuesta que `/v2/scrape` ([detalles](parse-api.md)):

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

## ¿Por qué PageSift?

| Característica | PageSift 0.51.0 | Firecrawl (Self-Hosted) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **Arquitectura Central** | 100% Rust Seguro (`forbid(unsafe)`) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **Compatibilidad API** | **Firecrawl v1+v2 + Jina + MCP** | Solo Firecrawl API | API personalizada Python | API personalizada REST |
| **Rendimiento de Extracción** | **3,525 páginas/seg** | ~150 páginas/seg | ~90 páginas/seg | ~1,200 páginas/seg |
| **RAM Pico (100k Dominios)** | **188.4 MB (Línea plana)** | > 8 GB (Colas Redis) | > 10 GB (Pool navegadores) | **8,978 MB (Fuga masiva 47.6×)** |
| **Tasa Conversión a Markdown** | **96.38% (Gran fidelidad)** | ~85% | ~88% | 82.82% (17.2% vacíos) |
| **Ajuste Adaptativo Hardware**| **$\min(CPU, RAM, FD)$ Auto** | Trabajadores manuales | Semáforo manual | Semáforo estático |
| **Autorreparación DOM** | **93.8% (0.0% Falsos Pos)** | ❌ No | ❌ No | ❌ No |
| **Extracción PDF y LaTeX** | **71% Aprobado / 44.3% LaTeX** | Solo texto externo | Solo texto sin formato | Solo texto sin formato |
| **Particionado RAG y BM25** | **19.7 ms / 1.35 ms BM25** | ❌ No | Chunkers Python (804 ms) | ❌ No |
| **Filosofía Anti-Bloqueo** | Ético, cortés, UA transparente | Rotación de proxies | Evasiones stealth | Evasiones stealth |

> Nota: la tabla mezcla versiones y conjuntos de datos. 3,525 páginas/s es de la época 0.40; 188.4 MB, 96.38 % (páginas con ≥100 B de Markdown), 93.8 % (500 casos) y 71 % son de 0.50; las cifras de la competencia («~150 / ~90 / ~1,200 páginas/s», «> 8 GB», «> 10 GB», «~85 % / ~88 %») no tienen medición en este repositorio: son estimaciones heredadas de un README anterior, **sin verificar**. Las comparaciones medidas están en [`benchmarks.md`](benchmarks.md) y [`benchmarks/`](benchmarks/).

---

## Comunidad

¿Dudas o comentarios? Únete a nuestro [Discord](https://discord.gg/esA3wMZVvA). La beta cerrada de la API alojada (próximamente, aún no disponible) también se anunciará allí.

---

## Licencia y Opciones Comerciales

- **Código Abierto:** Bajo licencia [AGPL-3.0](LICENSE).
- **Licenciamiento Comercial:** ¿Requiere enlaces propietarios o despliegues privados sin obligaciones AGPL? Consulte nuestra [Guía de Licencias Comerciales](COMMERCIAL.md) o contacte a `licensing@pagesift.dev`.
