"""Entry point: parse a CSV of expenses and print a per-category report."""

import sys

from parser import parse_rows
from report import render_report


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: main.py <expenses.csv>", file=sys.stderr)
        return 2
    # TODO: accept '-' for stdin
    with open(sys.argv[1], encoding="utf-8") as f:
        rows = parse_rows(f)
    print(render_report(rows))
    return 0


if __name__ == "__main__":
    sys.exit(main())
