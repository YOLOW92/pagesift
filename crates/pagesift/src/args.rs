//! 命令行子命令与参数定义。

use crate::*;

/// 细粒度内网放行：HTTP 抓取和浏览器子请求共用同一份名单。
#[derive(clap::Args)]
pub(crate) struct AllowCidrs {
    /// Allow private CIDR subnets (e.g. 10.0.0.0/8, 198.18.0.0/15) for HTTP fetching and browser rendering
    #[arg(long = "allow-cidr", value_name = "CIDR", value_parser = parse_cidr)]
    allow_cidrs: Vec<pagesift_net::IpCidr>,
}

impl AllowCidrs {
    pub(crate) fn apply(&self, config: &mut pagesift::ScraperConfig) {
        config
            .fetcher
            .ssrf
            .allow_cidrs
            .extend(self.allow_cidrs.iter().cloned());
    }
}

pub(crate) fn parse_cidr(raw: &str) -> Result<pagesift_net::IpCidr, String> {
    raw.parse()
        .map_err(|error| format!("invalid CIDR {raw:?}: {error}"))
}

#[derive(Subcommand)]
pub(crate) enum ModelsCommand {
    /// Download the OCR models for offline use, from their pinned upstream versions, and verify their SHA-256
    Fetch {
        /// Which models: auto (what `[ocr] quality` would use), fast (small) or accurate (medium)
        #[arg(long)]
        quality: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum GraphCommand {
    /// Inspect DataGraph artifacts in a crawl `--out` directory, graph JSON/JSONL file, or data store directory
    Inspect {
        /// Path to `--out` directory, `graph_summary.json` / `graph.jsonl` file, or store data directory
        path: PathBuf,
        /// Filter version history by URL or normalized resource key (for store or delta inspection)
        #[arg(long)]
        url: Option<String>,
        /// Filter recorded conflicts by entity subject key (e.g. sku:PRO-1)
        #[arg(long)]
        subject: Option<String>,
        /// Output raw inspection result as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Inspect local form metadata and redacted technology/secret indicators
    Inspect {
        file: Option<PathBuf>,
        #[arg(long)]
        base_url: Url,
    },
    /// Find an element again after a layout change (selector, identity attribute, then structural
    /// fingerprint with an outlier test); reports ambiguous instead of guessing
    Relocate {
        file: PathBuf,
        #[arg(long)]
        original: PathBuf,
        #[arg(long)]
        selector: String,
        /// Stable business identity attribute (e.g. data-sku): when set, a mismatch is reported as
        /// missing instead of falling back to similarity
        #[arg(long)]
        identity: Option<String>,
    },
    /// Discover same-origin external JS modules and static navigation candidates
    DiscoverJs {
        url: Url,
        #[arg(long, default_value_t = 32)]
        max_files: usize,
        #[arg(long, default_value_t = 3)]
        max_depth: usize,
    },
    /// Extract original media URLs with an explicit site/protocol adapter
    MediaExtract {
        file: Option<PathBuf>,
        #[arg(long)]
        base_url: Url,
        #[arg(long)]
        adapter: pagesift::media::MediaAdapter,
    },
    /// Archive a media API response, then publish named files and metadata
    MediaArchive {
        url: Url,
        #[arg(long)]
        adapter: pagesift::media::MediaAdapter,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "{id}-{hash}.{ext}")]
        template: String,
        #[arg(long, default_value_t = 268_435_456)]
        max_bytes: u64,
    },
    /// Plan finite unencrypted HLS/DASH without downloading segments
    StreamPlan {
        file: Option<PathBuf>,
        #[arg(long)]
        base_url: Url,
    },
    /// Archive finite HLS/DASH locally (no A/V remux)
    StreamDownload {
        url: Url,
        #[arg(long)]
        out: PathBuf,
        /// Select this zero-based HLS master variant; no automatic selection
        #[arg(long)]
        variant: Option<usize>,
        #[arg(long, default_value_t = 268_435_456)]
        max_bytes: u64,
    },
    /// Convert a local HTML file (or stdin) to Markdown
    Convert {
        /// HTML file to read; reads stdin when omitted or "-"
        file: Option<PathBuf>,
        /// URL the HTML was fetched from, used to make links absolute
        #[arg(long)]
        base_url: Option<Url>,
        /// Keep the whole page instead of only the main content
        #[arg(long)]
        full: bool,
        /// Decode the input with this charset (e.g. Shift_JIS, GBK) instead of detecting it
        #[arg(long)]
        charset: Option<String>,
        /// Write the Markdown file and an index.jsonl line into this directory (same layout as
        /// crawl --out) instead of printing
        #[arg(long)]
        out: Option<PathBuf>,
        /// Export the images the page references (fetched politely relative to --base-url) into
        /// <out>/images/<hash>.<ext> and point the Markdown links there
        #[arg(long, requires = "out")]
        export_images: bool,
    },
    /// Incrementally collect a JSON API; emit acknowledged upsert/delete events
    Collect {
        url: Url,
        #[arg(long)]
        source: String,
        #[arg(long)]
        data_dir: PathBuf,
        /// Durable append-only event log (replays may contain duplicate events)
        #[arg(long)]
        out: PathBuf,
        /// Abandon an incomplete scan and restart; committed inventory is retained
        #[arg(long)]
        restart: bool,
        /// Adapter JSON file; omitted for a WordPress REST collection
        #[arg(long)]
        adapter: Option<PathBuf>,
        /// Declare the listing complete and authoritative for deletion sync
        #[arg(long)]
        authoritative: bool,
        #[arg(long, default_value_t = 100)]
        max_pages: usize,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Archive one direct media URL with validated resume and a stable identity
    Download {
        url: Url,
        #[arg(long)]
        source: String,
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "1")]
        revision: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 268_435_456)]
        max_bytes: u64,
        #[arg(long)]
        expected_blake3: Option<String>,
        /// Discard an incomplete transfer; completed items remain immutable
        #[arg(long)]
        restart: bool,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Fetch one HTTP page with an explicit persistent, origin-isolated cookie session
    SessionFetch {
        url: Url,
        #[arg(long)]
        state: PathBuf,
        /// Explicitly replace stored cookies and retired/expired session state
        #[arg(long)]
        reset_session: bool,
        #[arg(long, default_value = "md", value_parser = ["md", "json"])]
        format: String,
        #[arg(long)]
        full: bool,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Start the Firecrawl-compatible HTTP API
    Serve {
        /// Port (default: server.port in the config, else 3002)
        #[arg(long)]
        port: Option<u16>,
        /// Address to listen on (default: server.host in the config, else 127.0.0.1)
        #[arg(long)]
        host: Option<String>,
        /// Require `Authorization: Bearer <key>` (repeatable); also read from PAGESIFT_API_KEYS (comma separated)
        #[arg(long = "api-key")]
        api_keys: Vec<String>,
        /// Public base URL used in job links (default: from the Host header)
        #[arg(long)]
        public_url: Option<String>,
        /// Where the cache lives (default: $PAGESIFT_DATA_DIR, else ~/.local/share/pagesift)
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Disable the result cache
        #[arg(long)]
        no_cache: bool,
        /// Deployment role (default: cluster.role in the config, else standalone)
        #[arg(long, value_parser = ["standalone", "all", "gateway", "worker", "standby"])]
        role: Option<String>,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Scrape a single URL
    Scrape {
        url: String,
        /// Output: md prints the Markdown, json prints the Firecrawl response
        #[arg(long, default_value = "md", value_parser = ["md", "json"])]
        format: String,
        /// http, browser or auto
        #[arg(long, default_value = "auto", value_parser = ["http", "browser", "auto"])]
        engine: String,
        /// Keep the whole page instead of only the main content
        #[arg(long)]
        full: bool,
        /// Ignore robots.txt for this page
        #[arg(long = "ignore-robots-txt", alias = "ignore-robots")]
        ignore_robots: bool,
        /// Egress proxy URL (e.g. http://127.0.0.1:7890, socks5://127.0.0.1:1080)
        #[arg(long)]
        proxy: Option<String>,
        /// Write the Markdown file and an index.jsonl line into this directory (same layout as
        /// crawl --out) instead of printing
        #[arg(long)]
        out: Option<PathBuf>,
        /// Also write the page to this WARC file (.warc, or .warc.gz for gzip members)
        #[arg(long)]
        warc: Option<PathBuf>,
        /// Print how the result was produced (render and merge decisions with their inputs) to
        /// stderr; also in metadata.pagesift.plan with --format json
        #[arg(long)]
        explain: bool,
        /// Export images (PDF embedded images, Office media, web page images fetched politely) as
        /// files: with --out they go to images/<hash>.<ext> and Markdown links point there
        #[arg(long)]
        export_images: bool,
        /// Image assets: attach Base64, embed data URIs, or rewrite to asset:// references
        #[arg(long, value_parser = ["attach", "embed", "rewrite"])]
        image_assets: Option<String>,
        /// Enable per-page DataGraph extraction (provenance, citations, blocks, evidence, claims)
        #[arg(long)]
        graph: bool,
        /// Entity subject keys for DataGraph correlation (comma-separated, e.g. sku,product_id)
        #[arg(long, value_delimiter = ',')]
        subject_keys: Vec<String>,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Research a question on the web with the configured LLM: search, read pages, stop when new
    /// pages add nothing. Prints one JSON object (data, sources, fieldSources); needs [llm], and
    /// [search] unless --urls is given. Exits 1 when the result is failed or incomplete.
    Agent {
        prompt: String,
        /// Pages to start from
        #[arg(long, num_args = 1..)]
        urls: Vec<String>,
        /// JSON Schema of the answer (file)
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Budget ceiling: pages, model calls and tokens
        #[arg(long, value_parser = ["low", "medium", "high"])]
        effort: Option<String>,
        /// Open only the pages in --urls
        #[arg(long)]
        strict_constrain_to_urls: bool,
        /// Stop after this many pages, searches and maps
        #[arg(long)]
        max_credits: Option<u64>,
        /// Open only pages on these hosts (and their subdomains)
        #[arg(long, num_args = 1..)]
        allowed_hosts: Vec<String>,
        /// Add the archived evidence text to the output
        #[arg(long)]
        include_evidence: bool,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Find the structural siblings of one example element (the whole list from one item);
    /// prints one JSON object (selector, depth, items)
    Similar {
        url: String,
        /// CSS selector of the example element (the first match is used)
        #[arg(long)]
        selector: String,
        /// http, browser or auto
        #[arg(long, default_value = "auto", value_parser = ["http", "browser", "auto"])]
        engine: String,
        /// Ignore robots.txt for this page
        #[arg(long = "ignore-robots-txt", alias = "ignore-robots")]
        ignore_robots: bool,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Extract structured data with a CSS schema (baseSelector, fields); one JSON line per URL.
    /// With a store ([cache], on by default) selectors are remembered and relocated after a page
    /// redesign; when another process holds the data directory it warns and runs without memory
    Extract {
        #[arg(required = true)]
        urls: Vec<String>,
        /// The schema file (JSON)
        #[arg(long)]
        schema: PathBuf,
        /// http, browser or auto
        #[arg(long, default_value = "auto", value_parser = ["http", "browser", "auto"])]
        engine: String,
        /// Ignore robots.txt for these pages
        #[arg(long = "ignore-robots-txt", alias = "ignore-robots")]
        ignore_robots: bool,
        /// Pages in flight at once; by default the admission controller's current target
        /// (per-host politeness limits apply either way)
        #[arg(long)]
        concurrency: Option<usize>,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Crawl a site; writes one Markdown file per page with --out, else JSON lines to stdout
    Crawl {
        url: String,
        /// Maximum number of pages (default: 100; with --adaptive the crawl decides when to stop,
        /// up to 500)
        #[arg(long)]
        limit: Option<usize>,
        /// Directory for the Markdown files, an index.jsonl (one line per page) and a summary.json
        /// (why the crawl stopped; coverage with --adaptive)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Also write every page to this WARC file (.warc, or .warc.gz for gzip members)
        #[arg(long)]
        warc: Option<PathBuf>,
        /// Maximum discovery depth from the start page
        #[arg(long = "max-depth", alias = "depth")]
        depth: Option<usize>,
        /// Only follow paths matching this regex (repeatable)
        #[arg(long)]
        include: Vec<String>,
        /// Skip paths matching this regex (repeatable)
        #[arg(long)]
        exclude: Vec<String>,
        /// Crawl the whole domain, not just pages under the start path (same as --scope domain)
        #[arg(long)]
        entire_domain: bool,
        /// Path scope: auto (start directory, widened one level when the start page is a leaf
        /// such as a blog post), exact (never widen), parent (one level up) or domain
        /// (default: auto; domain with --query)
        #[arg(long, value_parser = ["auto", "exact", "parent", "domain"])]
        scope: Option<String>,
        /// Follow links to subdomains
        #[arg(long)]
        subdomains: bool,
        /// include, skip or only
        #[arg(long, default_value = "include", value_parser = ["include", "skip", "only"])]
        sitemap: String,
        /// http, browser or auto
        #[arg(long, default_value = "auto", value_parser = ["http", "browser", "auto"])]
        engine: String,
        /// Ignore robots.txt. On the command line this flag is the permission itself; the HTTP API
        /// also needs `allow_crawl_ignore_robots = true` in the config (the operator's switch)
        #[arg(long = "ignore-robots-txt", alias = "ignore-robots")]
        ignore_robots: bool,
        /// Follow bounded same-origin external JS candidates (requires --engine http)
        #[arg(long)]
        external_scripts: bool,
        /// Crawl the pages most relevant to this query first
        #[arg(long)]
        query: Option<String>,
        /// With --query: decide how many pages to crawl. Stops once the pages cover the query and
        /// stop adding anything new, or when nothing relevant turns up; --limit is then only a cap
        #[arg(long, requires = "query")]
        adaptive: bool,
        /// With --query: also fetch and convert PDF/Office documents linked from relevant pages
        #[arg(long, requires = "query")]
        attachments: bool,
        /// Maximum number of attachments (default: same as --limit)
        #[arg(long, requires = "attachments")]
        attachment_limit: Option<usize>,
        /// Ignored (kept so old scripts still run). Concurrency now adapts online: the global limit
        /// follows the measured scalability curve (USL) and each site follows its latency gradient
        #[arg(long, hide = true)]
        tuning_mode: Option<String>,
        /// Export images (PDF embedded images, Office media, web page images fetched politely) as
        /// files: with --out they go to images/<hash>.<ext> and Markdown links point there
        #[arg(long)]
        export_images: bool,
        /// Image assets: attach Base64, embed data URIs, or rewrite to asset:// references
        #[arg(long, value_parser = ["attach", "embed", "rewrite"])]
        image_assets: Option<String>,
        /// Read the pages from this WARC file (.warc or .warc.gz) instead of the network: archived
        /// pages on the start URL's host that pass --include/--exclude, up to --limit; links are
        /// not followed
        #[arg(long)]
        from_warc: Option<PathBuf>,
        /// Enable cross-source DataGraph (writes graph.jsonl and graph_summary.json with --out)
        #[arg(long)]
        graph: bool,
        /// Entity subject keys for cross-source correlation and conflict detection (comma-separated)
        #[arg(long, value_delimiter = ',')]
        subject_keys: Vec<String>,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Print the detected hardware and the cold-start concurrency as JSON (tuning itself happens online)
    Tune {
        /// Ignored; kept for compatibility
        #[arg(long, default_value = "extreme", value_parser = ["standard", "aggressive", "extreme"])]
        profile: String,
        /// File to write the JSON profile to (prints to stdout if omitted)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Show CPU topology, memory budget, file-descriptor headroom and the concurrency limits
    Diagnose {
        /// Also show the fitted scalability curve (USL), when there are samples
        #[arg(long)]
        performance: bool,
        /// Output diagnosis report in JSON format
        #[arg(long)]
        json: bool,
    },
    /// List the URLs of a site
    Map {
        url: String,
        #[arg(long, default_value_t = 5000)]
        limit: usize,
        /// Rank URLs matching these words first
        #[arg(long)]
        search: Option<String>,
        /// include, skip or only
        #[arg(long, default_value = "include", value_parser = ["include", "skip", "only"])]
        sitemap: String,
        /// Ignore robots.txt. On the command line this flag is the permission itself; the HTTP API
        /// also needs `allow_crawl_ignore_robots = true` in the config (the operator's switch)
        #[arg(long = "ignore-robots-txt", alias = "ignore-robots")]
        ignore_robots: bool,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Run the MCP server (stdio by default)
    Mcp {
        /// stdio or http
        #[arg(default_value = "stdio")]
        transport: String,
        /// Port for the http transport (listens on 127.0.0.1)
        #[arg(long, default_value_t = 3003)]
        port: u16,
    },
    /// Check the environment
    Doctor,
    /// Manage the OCR models
    Models {
        #[command(subcommand)]
        action: ModelsCommand,
    },
    /// Inspect persisted or exported DataGraph outputs
    Graph {
        #[command(subcommand)]
        action: GraphCommand,
    },
    /// Evaluate content extraction on local dataset snapshots
    Bench {
        /// Directory holding 1-0-0.csv and snapshots/ (see benchmarks/common/fetch_snapshots.py)
        dir: PathBuf,
        /// Write per-page results as JSON lines
        #[arg(long)]
        out: Option<PathBuf>,
        /// Timing runs per page; the fastest is reported
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// Only evaluate the first N dataset rows
        #[arg(long)]
        limit: Option<usize>,
        /// Simulate `engine: auto` using browser-rendered snapshots (see bench-render)
        #[arg(long)]
        auto: bool,
        /// Write the HTML each page was evaluated on, plus a manifest, for other tools
        #[arg(long)]
        emit_inputs: Option<PathBuf>,
        /// Score another tool's output (JSON lines with id, markdown, ms) instead of pagesift
        #[arg(long)]
        external: Option<PathBuf>,
        /// Threads for the throughput run; 0 uses every core
        #[arg(long, default_value_t = 0)]
        jobs: usize,
    },
    /// Render thin snapshots with a browser for `bench --auto`
    #[command(hide = true)]
    BenchRender {
        dir: PathBuf,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value_t = 8)]
        concurrency: usize,
    },
}
