"""pagesift MCP server entry point."""

import argparse
import sys
from pagesift._pagesift import run_mcp


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="pagesift-mcp",
        description="Run the pagesift Model Context Protocol (MCP) server.",
    )
    parser.add_argument(
        "--transport",
        choices=["stdio", "http"],
        default="stdio",
        help="Transport protocol: 'stdio' (default) or 'http'",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=3001,
        help="HTTP port when using --transport http (default: 3001)",
    )
    args = parser.parse_args()

    try:
        run_mcp(transport=args.transport, port=args.port)
    except KeyboardInterrupt:
        sys.exit(0)


if __name__ == "__main__":
    main()
