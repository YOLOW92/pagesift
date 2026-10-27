# Secciones retiradas de la portada (español)

> Estas secciones estuvieron en la portada del proyecto hasta 0.80.0 y se trasladaron aquí sin cambios (solo se ajustaron los enlaces relativos). Describen versiones anteriores; los resultados actuales están en [benchmarks.md](../benchmarks.md).

## Novedades en 0.73.0

0.73.0 corrige lo que encontraron una revisión de código en tres frentes y unas 150 pruebas con entradas adversas.

- **La extracción vuelve a usar todos los núcleos**: desde 0.70.0, `scrape`, `crawl` y `serve` extraían en un solo núcleo; el servidor API atiende unas 3,5–7× más peticiones por segundo (16 clientes, motor `auto`, ejecuciones alternas en un i7-10700: 309–336 → 1.153–2.196 req/s; p50 de 36–45 ms a 4,5–6,1 ms).
- **Sin los fallos ni vías de DoS encontrados**: pánico en páginas CJK, páginas UTF-8 convertidas en mojibake, análisis cuadrático de atributos (40.000 atributos: 3 s → 0,02 s), clientes slowloris y enlaces `javascript:` en elementos RSS.
- **Heurísticas acotadas**: los artículos cortos de Substack se marcaban como verificación de edad; widgets de autores, raíces de aplicación con cloak, contenido de `<template>` y variables CSS tomadas por claves de OpenAI se tratan correctamente.
- **Límites y tiempos de espera en la API** para cada parámetro, con el cuerpo de error JSON estándar en todas partes.
- **Alcance de crawl decidido por evidencias** (`--scope auto`, por defecto): `/docs/python` se queda en `/docs/python/*`; una semilla hoja, como un artículo de blog, se amplía a su sección y lo avisa.

La calidad se mantiene o mejora (WCXB F1 0,8522 → 0,8533; Firecrawl 0,680 → 0,682; AEB 0,958) y la extracción es un 3–5% más rápida en páginas reales. Detalle: [notas de 0.73.0](../releases/0.73.0.md).

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

Detalle: [notas de 0.72.8](../releases/0.72.8.md).

---

## Novedades en 0.70.0

La versión 0.70 reconstruye el flujo de datos HTTP con ejecutores locales a NUMA, un reactor HTTP/1.1 ligero, conexiones adaptativas por origen y procesamiento fragmentado con límites. El motor usa `memory.max` de cgroup v2 para ajustar la capacidad: con un límite de 768 MiB se midieron 380 conexiones, 817.346 páginas/s y ningún OOM.

- **Prueba de saturación sintética de 200 millones de tareas en 192 vCPU**: 200.000.000/200.000.000 completadas, cero fallos, 947.611 páginas/s, 211,1 s y 888,1 MiB de RSS máximo. Esta cifra corresponde a carga sintética, no a rastreo de la web pública.
- **Resultados públicos que incluyen las derrotas**: PageSift lidera 4 de 7 dominios; las diferencias en extracción de contenido, renderizado dinámico y autorreparación DOM figuran abajo.
- **Correcciones operativas**: `timeout_secs` limita también la espera de cabeceras; se recuperan las penalizaciones 429; una URL fallida ya no bloquea todo el host; se evita el panic con un `ulimit -v` bajo.

El diseño, las condiciones de medición y la migración están en las [notas de la versión 0.70.0](../releases/0.70.0.md).

---

## 🏆 Comparación pública en siete dominios (0.70, medida en octubre de 2026; no repetida en versiones posteriores)

Todas las cifras proceden de **ejecuciones completas en la misma máquina**, con el evaluador propio de cada conjunto y un límite de memoria de 10 GiB. Equipo: Intel i7-10700 (8C/16T), 15 GB de RAM y Ubuntu 26.04. Los scripts están en [`benchmarks/`](../../benchmarks/) y los informes por dominio en [`docs/history/`](./).

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
| Páginas Crudas 200 OK        | 62,937 páginas          | 72,209 páginas         | Spider
| Markdown Válido (>100B)      | 60,659 documentos       | 59,803 documentos      | PageSift (+856 docs)
| Páginas con ≥100B de Markdown| 96.38%                  | 82.82%                 | PageSift
| Memoria RAM Pico (RSS)       | 188.4 MB                | 8,978.9 MB             | PageSift (8,978.9 ÷ 188.4 ≈ 47.7)
| Escala de millones           | No medido (extrapolación)| No medido (extrapolación)| —

+-----------------------------------------------------------------------------------------+
```

> **Notas (0.50, una sola ejecución; datos históricos, no repetidos con la versión actual)**
> - **Memoria**: el pico de PageSift fue de **188.4 MB** y el de Spider de **8,978.9 MB** (alcanzado en unos 2,5 minutos). Spider también completó las 100,000 URL: «eliminado por OOM» o «cuelgue a 14 GB» no son resultados de esta ejecución (el comportamiento a 1 millón es una extrapolación, no una medición).
> - **Velocidad**: Spider es más rápido (666.6 QPS sostenidos frente a 93.5; 72,209 respuestas 200 OK frente a 62,937).
> - **Extracción**: la «tasa de conversión» es la proporción de páginas 200 OK para las que el extractor de cada herramienta devolvió ≥ 100 bytes de Markdown (PageSift 96.38 %, Spider 82.82 %); no mide fidelidad. Documentos válidos: 60,659 frente a 59,803.
> - Informe: [`benchmarks/history/0.50/results/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md`](https://github.com/YOLOW92/pagesift/blob/main/benchmarks/history/0.50/results/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md).

---

## 🏆 Matriz de los 10 dominios (datos históricos de 0.50, no repetidos con la versión actual)

Cada cifra de la tabla procede de la tabla de mediciones del texto del informe correspondiente en `benchmarks/history/0.50/results/` (0.50, en torno al 23 de octubre de 2026, una sola ejecución); la columna «Fuente» indica el informe y la sección. Hardware según los informes: D1–D5 y la comparación de 100k, AMD EPYC 9B45 (32 núcleos, 128 GB); D6–D10, AMD EPYC 7B13 (`c2d-standard-32`). Los cocientes son siempre la división directa de dos cifras medidas, y se indica cuáles. ↑ más alto es mejor, ↓ más bajo es mejor.
Las cifras de la tabla anterior venían de CSV generados con valores fijos por `bench/run_full_10_benchmarks.py` (el script no ejecutaba ninguna prueba); el script y esos CSV se han eliminado.

| Dominio | PageSift 0.50 (medido en el informe) | Competidores (medidos en el mismo informe) | Cociente (división de dos cifras medidas) | Memoria pico ↓ (PageSift frente a competidor) | Fuente |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **01. Extracción de contenido** | WCXB, 2,008 páginas: **6,273.9 páginas/s** ↑ con 16 hilos (8,833.8 con 32); media por página con un hilo 1.30 ms ↓¹ | El informe no mide el rendimiento multihilo de los competidores. Media por página con un hilo ↓: trafilatura 2.3.1 26.52 ms, Crawl4AI 0.9.5 283.38 ms | Media por página 26.52 ÷ 1.30 ≈ 20.4 (trafilatura ÷ PageSift) | No medida en el informe | [D1](../../benchmarks/history/0.50/results/BENCHMARK_D1_CONTENT_EXTRACTION_050.md) §2, §4 |
| **02. 100k dominios reales / 50k URL sintéticas** | 100k: 62,937 respuestas 200 OK ↑, 60,659 con ≥ 100 B de Markdown ↑, 93.5 QPS ↑<br>50k sintéticas: 5,894.8 páginas/s ↑² | Spider 2.53.9, 100k: 72,209 respuestas 200 OK, 59,803 con ≥ 100 B de Markdown, 666.6 QPS<br>50k sintéticas: 6,532.3 páginas/s | **Spider es más rápido**: 666.6 ÷ 93.5 ≈ 7.1; 6,532.3 ÷ 5,894.8 ≈ 1.11<br>Memoria: 8,978.9 ÷ 188.4 ≈ 47.7; 11,316.3 ÷ 502.4 ≈ 22.5 | 100k: 188.4 MB frente a 8,978.9 MB<br>50k: 502.4 MB frente a 11,316.3 MB | [Informe 100k](../../benchmarks/history/0.50/results/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md), tabla resumen; [D2](../../benchmarks/history/0.50/results/BENCHMARK_D2_MASSIVE_CRAWL_CHAOS_050.md) §2 |
| **03. Extracción de PDF** | olmOCR-bench, 1,269 páginas: **72.4%** de aciertos global ↑, fórmulas 70.4% ↑; 2,682.2 páginas/s ↑ con 32 hilos³ | Docling 2.142.0: **73.5% (más alto)**, 0.57 páginas/s<br>PyMuPDF4LLM 2.0: 64.4%, fórmulas 50.0%, 11.1 páginas/s | Rendimiento 2,682.2 ÷ 11.1 ≈ 242 (frente a PyMuPDF4LLM) | 359.6 MB frente a 11,663.1 MB (PyMuPDF4LLM) | [D3](../../benchmarks/history/0.50/results/BENCHMARK_D3_HEAVY_PDF_EXTRACTION_050.md) §2, §3 |
| **04. Autorreparación DOM** | 2,000 casos sintéticos: **98.85%** recuperados ↑ (1,977/2,000), 1.15% falsos positivos ↓, media 29.2 µs ↓⁴ | Scrapling 0.5: 92.95%, 7.05% falsos positivos, 990.8 µs<br>BS4 + lxml + RapidFuzz: 98.85% (empate), 2,102.9 µs | Tiempo medio 990.8 ÷ 29.2 ≈ 33.9 (Scrapling ÷ PageSift) | 63.2 MB frente a 1,330.9 MB (Scrapling) | [D4](../../benchmarks/history/0.50/results/BENCHMARK_D4_DOM_SELF_HEALING_050.md) §2 |
| **05. Particionado RAG y filtrado** | Chroma, 472 preguntas: recursive R@1000 **0.893** ↑, particionado 12.89 ms ↓<br>QueryFilter: 0.63 ms por pregunta ↓, recall conservado 0.929 ↑ | semchunk: R@1000 0.893, 65.60 ms; LangChain: **0.895 (más alto)**, 104.80 ms<br>Crawl4AI BM25ContentFilter: 838.00 ms por pregunta, recall conservado 0.673 | Particionado 65.60 ÷ 12.89 ≈ 5.1 (semchunk)<br>Filtrado 838.00 ÷ 0.63 ≈ 1,330 (Crawl4AI) | No medida en el informe | [D5](../../benchmarks/history/0.50/results/BENCHMARK_D5_RAG_CHUNKING_050.md) §2 |
| **06. Detección de codificación** | chardet-test-data, 3,128 archivos: **76.02%** de acierto ↑; 24,915.3 archivos/s ↑ con 32 hilos⁵ | chardet 7.7.0: 71.13%, 11,412.3 archivos/s con 32 procesos | Rendimiento 24,915.3 ÷ 11,412.3 ≈ 2.2 | No se muestra⁸ | [D6](../../benchmarks/history/0.50/results/BENCHMARK_D6_CHARSET_DETECTION_050.md) §3.1, §3.2 |
| **07. Datos estructurados** | WCXB, 2,008 páginas: 33,512.9 páginas/s ↑, p50 0.277 ms ↓; analiza anidamientos de 2,000 niveles⁶ | extruct 0.18.0: 3,449.1 páginas/s, p50 2.442 ms; `RecursionError` a partir de 500 niveles | Rendimiento 33,512.9 ÷ 3,449.1 ≈ 9.7 | No se muestra⁸ | [D7](../../benchmarks/history/0.50/results/BENCHMARK_D7_STRUCTURED_DATA_050.md) §3.1, §4.1 |
| **08. Descubrimiento de endpoints JS** | Conjunto sintético con señuelos, 500 archivos: precisión **100%** ↑ (1,000 endpoints reales, 0 señuelos)<br>2,000 paquetes JS reales: 160,287.0 archivos/s ↑⁷ | jsluice 1.0: precisión 40.0% (1,500 señuelos), 7,951.4 archivos/s<br>LinkFinder: precisión 66.7% | Rendimiento 160,287.0 ÷ 7,951.4 ≈ 20.2 (frente a jsluice) | No se muestra⁸ | [D8](../../benchmarks/history/0.50/results/BENCHMARK_D8_JS_ENDPOINTS_050.md) §3.1, §4.4 |
| **09. Renderizado dinámico (CDP)** | 100 renderizados de 5 tipos de SPA locales, todos correctos; **25.7 páginas/s** ↑, p50 3,337.56 ms ↓; supera el Shadow DOM de 50 niveles⁹ | Playwright 1.64.1: 41.6 páginas/s, p50 566.66 ms, supera el Shadow DOM<br>Lightpanda 1.0.2: 71.8 páginas/s, p50 29.92 ms, falla el Shadow DOM | **Playwright es más rápido**: 41.6 ÷ 25.7 ≈ 1.6 | Sin medición por herramienta | [D9](../../benchmarks/history/0.50/results/BENCHMARK_D9_DYNAMIC_HEADLESS_050.md) §3.1, §4.2 |
| **10. Gateway de API** | `/v2/scrape`, 1,000 peticiones: **308.0 req/s** ↑, p50 4.37 ms ↓ | Gateway de Crawl4AI, 50 peticiones: 33.7 req/s, p50 451.03 ms | 308.0 ÷ 33.7 ≈ 9.1 (distinto número de peticiones, muestra pequeña) | En reposo, unos 20.4 MB frente a unos 380 MB (cifras aproximadas del informe) | [D10](../../benchmarks/history/0.50/results/BENCHMARK_D10_AI_API_GATEWAYS_050.md) §3.1, §3.4 |

**Notas por fila (revisión del 27 de octubre de 2026)**
¹ La medición de 16 hilos en D1 §4 es 6,273.9 páginas/s; el 3,525 que aparece entre paréntesis en la misma fila es la cifra antigua del portátil con 0.40, que la tabla anterior usaba por error. El F1 0.8472 de §2 coincide con la puntuación WCXB de 0.70 con el evaluador oficial y el informe no explica cómo se obtuvo, así que no se muestra (la comparación WCXB en la misma máquina está en la tabla de siete dominios de arriba).
² La comparación de 100k es una sola ejecución y ambas herramientas terminaron; PageSift procesó 105,660 URL y Spider 100,000. «96.38 % frente a 82.82 %» es la proporción de páginas 200 OK para las que el extractor de cada herramienta devolvió ≥ 100 bytes de Markdown; no mide fidelidad. Las 50k URL son un sitio sintético local (D2 §2).
³ El informe dice «conjunto oficial completo», pero son los 5 subconjuntos de PDF nativos digitales de olmOCR-bench (los mismos que en [`benchmarks.md` §5](../benchmarks.md#5-pdf-to-markdown)); las puntuaciones globales de los competidores en el informe son idénticas a las medidas con 0.12. Con olmOCR-bench completo (1,403 PDF / 7,019 comprobaciones) en la misma máquina con 0.70: 0.4506 frente a 0.2446 de pymupdf4llm; fórmulas 0.3530 ([D3](0.70_showdown_D3.md)).
⁴ Conjunto sintético de mutaciones. En otro conjunto de 2,000 casos en la misma máquina (0.70): PageSift 0.8215, Scrapling 0.8875; 0.50 obtuvo 0.6965 ([D4](0.70_showdown_D4.md)), la conclusión contraria a esta fila.
⁵ En las cuatro familias de idiomas distintas de Unicode chardet acierta más (p. ej., occidentales 71.62 % frente a 50.00 %, D6 §3.2). La repetición de 0.70 en la misma máquina da la misma precisión (0.7602 frente a 0.7113) y una velocidad de 11,239 frente a 6,800 archivos/s ([D6](0.70_showdown_D6.md)).
⁶ Solo cuenta las entidades extraídas; no hay respuestas de referencia. La precisión está en [D7](0.70_showdown_D7.md) de 0.70 en la misma máquina (0.7392 frente a 0.5488 de bs4+lxml).
⁷ Con los 2,000 paquetes reales PageSift informó 400 endpoints, jsluice 1,041 y LinkFinder 2,840; sin respuestas de referencia no se puede juzgar el recall.
⁸ En los informes D6–D8 la memoria pico de todas las herramientas (incluidos los competidores con 32 procesos) es de solo unos 20–32 MB, lo que sugiere que solo se midió el proceso principal; como la medición es dudosa, no se muestra.
⁹ 0.70 en la misma máquina, 7 escenarios SPA: PageSift 3/7, Playwright 5/7 ([D9](0.70_showdown_D9.md)).

> [!TIP]
> Los JSON/JSONL en bruto de cada informe (`benchmark_d6_results.json`, `benchmark4_tier1_standard_2000.jsonl`, etc.) están en [`benchmarks/history/0.50/results/`](../../benchmarks/history/0.50/results/). Algunas cifras no coinciden con el texto del informe (p. ej., el rendimiento de D3 y D9); la tabla sigue el texto del informe.

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

## ¿Por qué PageSift? (cifras de distintas versiones; véanse las notas bajo la tabla)

| Característica | PageSift (versión y fuente en las notas) | Firecrawl (Self-Hosted) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **Arquitectura Central** | Rust; `forbid(unsafe_code)` en todos los crates salvo `pagesift-sys` (una llamada a `mallopt` de glibc) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **Compatibilidad API** | **Firecrawl v1+v2 + Jina Reader + MCP** | Solo Firecrawl API | API personalizada Python | API personalizada REST |
| **Rendimiento de extracción ↑** | 6,273.9 páginas/s (0.50, 16 hilos, WCXB, 2,008 páginas) ᵃ; en origen sintético local 59,309 (16 vCPU) / 947,611 (192 vCPU) ᵇ | No medido | No medido; media por página con un hilo 283.38 ms ↓ (PageSift 1.30 ms) ᵃ | No medido |
| **RAM pico (100k dominios) ↓** | **188.4 MB** (0.50) ᶜ | No medido | No medido | 8,978.9 MB (en la misma ejecución Spider fue más rápido, 666.6 frente a 93.5 QPS) ᶜ |
| **Páginas `200 OK` con ≥ 100 B de Markdown ↑** | 96.38% (0.50) ᶜ | No medido | No medido | 82.82% ᶜ |
| **Ajuste Adaptativo Hardware** | **$\min(CPU, RAM, FD)$ Auto** | Trabajadores manuales | Semáforo manual | Semáforo estático |
| **Autorreparación DOM (recuperación ↑)** | 0.8215 en un conjunto de 2,000 casos (0.70, por debajo de Scrapling, 0.8875); 98.85% en otro conjunto sintético de 2,000 casos del informe de 0.50 ᵈ | ❌ No | ❌ No | ❌ No |
| **Extracción de PDF (aciertos ↑)** | olmOCR-bench completo 0.4506 (0.70; pymupdf4llm 0.2446); 72.4% en 5 subconjuntos según el informe de 0.50 (Docling, 73.5%, es más alto) ᵈ | Solo texto externo | Solo texto sin formato | Solo texto sin formato |
| **Particionado RAG y filtrado (tiempo ↓)** | Particionado 12.89 ms / QueryFilter 0.63 ms por pregunta (0.50, Chroma, 472 preguntas) ᵉ | ❌ No | Filtro BM25 838.00 ms por pregunta ᵉ | ❌ No |
| **Filosofía Anti-Bloqueo** | Ético, cortés, UA transparente | Rotación de proxies | Evasiones stealth | Evasiones stealth |

ᵃ [`BENCHMARK_D1_CONTENT_EXTRACTION_050.md`](../../benchmarks/history/0.50/results/BENCHMARK_D1_CONTENT_EXTRACTION_050.md) §2, §4 (0.50, una ejecución).
ᵇ Origen sintético en la misma máquina, sin red ([`0.70_200m_regression.md`](0.70_200m_regression.md)); no es comparable con las demás celdas.
ᶜ [`BENCHMARK_100K_PAGESIFT_VS_SPIDER.md`](../../benchmarks/history/0.50/results/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md) (0.50, una ejecución). El porcentaje es la proporción de páginas para las que el extractor de cada herramienta devolvió ≥ 100 bytes; no mide fidelidad.
ᵈ Las cifras de 0.70 son las ejecuciones más recientes en la misma máquina ([D4](0.70_showdown_D4.md), [D3](0.70_showdown_D3.md)); las de 0.50 proceden del [informe D4](../../benchmarks/history/0.50/results/BENCHMARK_D4_DOM_SELF_HEALING_050.md) §2 y del [informe D3](../../benchmarks/history/0.50/results/BENCHMARK_D3_HEAVY_PDF_EXTRACTION_050.md) §2, con otros conjuntos de datos y conclusiones distintas.
ᵉ [`BENCHMARK_D5_RAG_CHUNKING_050.md`](../../benchmarks/history/0.50/results/BENCHMARK_D5_RAG_CHUNKING_050.md) §2 (0.50).
«No medido»: no hay ninguna medición de esa celda en el repositorio. Las cifras que había antes en esas celdas («~150 / ~90 / ~1,200 páginas/s», «> 8 GB / > 10 GB», «~85 % / ~88 %») eran estimaciones sin fuente y se han eliminado.
