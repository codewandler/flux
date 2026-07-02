"""Render the per-category expense report."""

from collections import defaultdict

from parser import Expense


def render_report(rows: list[Expense]) -> str:
    totals: dict[str, int] = defaultdict(int)
    for r in rows:
        totals[r.category] += r.amount_cents
    lines = ["category            total"]
    # TODO: right-align the amount column
    for cat, cents in sorted(totals.items(), key=lambda kv: -kv[1]):
        lines.append(f"{cat:<18} {cents / 100:.2f}")
    return "\n".join(lines)
