"""Execution entry point when invoking `python -m pagesift`."""

import sys


def main() -> None:
    if len(sys.argv) > 1 and sys.argv[1] == "mcp":
        sys.argv.pop(1)
        from pagesift.mcp import main as mcp_main

        mcp_main()
    else:
        from pagesift.cli import main as cli_main

        cli_main()


if __name__ == "__main__":
    main()
