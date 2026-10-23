"""pagesift Python CLI wrapper."""

import argparse
import json
import sys
from pagesift import convert
from pagesift._pagesift import __version__


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="pagesift",
        description=f"pagesift {__version__} - Clean Markdown from web pages for LLMs.",
    )
    parser.add_argument("-V", "--version", action="version", version=f"pagesift {__version__}")
    subparsers = parser.add_subparsers(dest="subcommand", help="Subcommand to run")

    # mcp
    mcp_parser = subparsers.add_parser("mcp", help="Run the MCP server (stdio by default)")
    mcp_parser.add_argument(
        "--transport",
        choices=["stdio", "http"],
        default="stdio",
        help="Transport mode (default: stdio)",
    )
    mcp_parser.add_argument(
        "--port",
        type=int,
        default=3001,
        help="HTTP port (default: 3001)",
    )

    # scrape
    scrape_parser = subparsers.add_parser("scrape", help="Scrape a single URL")
    scrape_parser.add_argument("url", help="Target URL to scrape")
    scrape_parser.add_argument(
        "--format",
        choices=["md", "json"],
        default="md",
        help="Output format: 'md' (default) or 'json'",
    )
    scrape_parser.add_argument(
        "--engine",
        choices=["http", "browser", "auto"],
        default="auto",
        help="Engine choice (default: auto)",
    )
    scrape_parser.add_argument(
        "--full",
        action="store_true",
        help="Keep full page instead of main content only",
    )
    scrape_parser.add_argument(
        "--proxy",
        help="Egress proxy URL (e.g. http://127.0.0.1:7890)",
    )
    scrape_parser.add_argument(
        "--allow-private-network",
        action="store_true",
        help="Allow scraping private/local addresses",
    )
    scrape_parser.add_argument(
        "--allow-cidr",
        action="append",
        dest="allow_cidrs",
        help="Allow specific CIDR subnet (repeatable)",
    )

    # convert
    convert_parser = subparsers.add_parser("convert", help="Convert local HTML to Markdown")
    convert_parser.add_argument("file", nargs="?", default="-", help="HTML file or '-' for stdin")
    convert_parser.add_argument("--url", help="Base URL for relative links")
    convert_parser.add_argument(
        "--full",
        action="store_true",
        help="Keep full page instead of main content only",
    )

    args = parser.parse_args()
    if args.subcommand == "mcp":
        from pagesift._pagesift import run_mcp
        try:
            run_mcp(transport=args.transport, port=args.port)
        except KeyboardInterrupt:
            sys.exit(0)
    elif args.subcommand == "scrape":
        from pagesift import Scraper
        scraper = Scraper(
            allow_private_network=args.allow_private_network,
            allow_cidrs=args.allow_cidrs,
            proxy=args.proxy,
        )
        try:
            doc = scraper.scrape(
                args.url,
                only_main_content=not args.full,
                engine=args.engine,
            )
            if args.format == "json":
                print(
                    json.dumps(
                        {
                            "success": True,
                            "data": {
                                "markdown": doc.markdown,
                                "metadata": doc.metadata,
                                "status_code": doc.status_code,
                                "url": doc.final_url or doc.url,
                            },
                        },
                        indent=2,
                        ensure_ascii=False,
                    )
                )
            else:
                sys.stdout.write(doc.markdown or "")
        except Exception as e:
            sys.stderr.write(f"Error: {e}\n")
            sys.exit(1)
        finally:
            scraper.close()
    elif args.subcommand == "convert":
        if args.file == "-":
            content = sys.stdin.read()
        else:
            with open(args.file, "r", encoding="utf-8") as f:
                content = f.read()
        doc = convert(content, url=args.url, only_main_content=not args.full)
        sys.stdout.write(doc.markdown or "")
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
