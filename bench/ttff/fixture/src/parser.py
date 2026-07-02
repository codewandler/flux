"""Permissive CSV row parser: bad rows are skipped, never fatal."""

import csv
from dataclasses import dataclass


@dataclass
class Expense:
    category: str
    amount_cents: int
    note: str


def parse_rows(lines) -> list[Expense]:
    out: list[Expense] = []
    for row in csv.reader(lines):
        if len(row) < 2:
            continue  # TODO: count skipped rows and warn above a threshold
        try:
            cents = round(float(row[1]) * 100)
        except ValueError:
            continue
        note = row[2] if len(row) > 2 else ""
        # TODO: normalize category casing before grouping
        out.append(Expense(category=row[0].strip(), amount_cents=cents, note=note))
    return out
