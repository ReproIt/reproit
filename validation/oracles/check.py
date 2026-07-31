#!/usr/bin/env python3
"""Fail-closed validator for the per-runner UI oracle coverage ledger.

A runner that emits no marker for an oracle reports no finding for it, and a
report with no finding reads exactly like a clean result. That is the one
difference this product exists to prevent, so `coverage.json` must state every
(oracle, runner) pair, and this validator proves each statement against the
runner sources rather than trusting the prose.

Run:

    python3 validation/oracles/check.py
    python3 validation/oracles/check.py --write
    python3 validation/oracles/check.py --check-generated
"""

import argparse
import json
import os
import re
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
LEDGER = os.path.join(ROOT, "validation", "oracles", "coverage.json")
REGISTRY = os.path.join(ROOT, "crates", "reproit", "oracle-registry.json")
PLATFORMS = os.path.join(ROOT, "crates", "reproit", "src", "adapters", "platform.rs")
PARSE_DIR = os.path.join(ROOT, "crates", "reproit", "src", "domain", "map")
DOC = os.path.join(ROOT, "docs", "oracles.md")
BEGIN = "<!-- generated:oracle-coverage -->"
END = "<!-- /generated:oracle-coverage -->"

# Markers the map parser consumes that are NOT per-oracle findings, each with
# the reason it carries no coverage row. Anything else the parser learns to read
# must be classified here or given a row, so a new marker cannot arrive silently.
STRUCTURAL_MARKERS = {
    "EXPLORE:STATE": "a visited state, the exploration graph's node",
    "EXPLORE:EDGE": "a state transition, the exploration graph's edge",
    "EXPLORE:GROUNDTRUTH": "the runner's own ground-truth dump, not a finding",
    "EXPLORE:RELATIONSTATUS": "the tri-state outcome channel for EXPLORE:RELATION",
    "EXPLORE:COVERAGE": "how much of the authored plan a walk reached",
    "EXPLORE:TRUNCATED": "the walk hit a budget, so the map is partial",
    "EXPLORE:UNSCANNABLE": "the target could not be reached at all (bot wall)",
}

STATES = ("evaluated", "unavailable", "unimplemented")


class Failure(Exception):
    pass


def load(path):
    with open(path, encoding="utf8") as handle:
        return json.load(handle)


def read(path):
    with open(path, encoding="utf8", errors="ignore") as handle:
        return handle.read()


def parsed_markers():
    """Every EXPLORE marker the Rust map parser reads."""
    markers = set()
    for dirpath, _dirs, files in os.walk(PARSE_DIR):
        for name in sorted(files):
            if not name.endswith(".rs"):
                continue
            for match in re.finditer(r'"(EXPLORE:[A-Z0-9]+)', read(os.path.join(dirpath, name))):
                markers.add(match.group(1))
    return markers


def registered_platforms():
    """Platform ids from the two tables in adapters/platform.rs."""
    text = read(PLATFORMS)
    ids = set()
    for table, pattern in (
        ("STATIC_PLATFORMS", r'Backend::[A-Za-z]+,\s*\n\s*"([a-z-]+)"'),
        ("DESKTOP_TOOLKITS", r'\(\s*\n\s*"([a-z-]+)",'),
    ):
        start = text.index("const %s" % table)
        end = text.index("\n];", start)
        ids.update(match.group(1) for match in re.finditer(pattern, text[start:end]))
    if not ids:
        raise Failure("could not read any platform id from %s" % PLATFORMS)
    return ids


def emits(source, marker):
    """Does this runner's source emit `marker`?

    Runners build the marker name by concatenation on the jank/hang path
    (`'EXPLORE:' + (kind === 'hang' ? 'HANG' : 'JANK')`), so the bare quoted
    name counts too.
    """
    name = marker.split(":", 1)[1]
    patterns = [re.escape(marker), "'%s'" % name, '"%s"' % name]
    for dirpath, _dirs, files in os.walk(os.path.join(ROOT, source)):
        for filename in sorted(files):
            text = read(os.path.join(dirpath, filename))
            if any(re.search(pattern, text) for pattern in patterns):
                return os.path.relpath(os.path.join(dirpath, filename), ROOT)
    return None


def validate(ledger):
    errors = []
    if ledger.get("schemaVersion") != 1:
        raise Failure("unsupported schemaVersion")
    runners = ledger["runners"]
    reasons = ledger["reasons"]
    oracle_ids = set(load(REGISTRY)["oracles"])

    covered_platforms = set()
    for name, runner in runners.items():
        if not os.path.isdir(os.path.join(ROOT, runner["source"])):
            errors.append("runner %s: missing source %s" % (name, runner["source"]))
        if not runner["platforms"]:
            errors.append("runner %s: claims no platform" % name)
        covered_platforms.update(runner["platforms"])
    missing = sorted(registered_platforms() - covered_platforms)
    if missing:
        errors.append("registered platforms with no runner row: %s" % ", ".join(missing))

    rows = {row["marker"]: row for row in ledger["oracles"]}
    if len(rows) != len(ledger["oracles"]):
        errors.append("duplicate marker rows")
    known = parsed_markers()
    for marker in sorted(known - set(rows) - set(STRUCTURAL_MARKERS)):
        errors.append("%s is parsed but has no coverage row" % marker)
    for marker in sorted(set(rows) - known):
        errors.append("%s has a coverage row but no parser reads it" % marker)

    used_reasons = set()
    for marker, row in sorted(rows.items()):
        if row["oracle"] not in oracle_ids:
            errors.append("%s: unknown oracle id %s" % (marker, row["oracle"]))
        seen = set()
        for runner in sorted(runners):
            states = [
                state
                for state, holder in (
                    ("evaluated", row["evaluated"]),
                    ("unavailable", row["unavailable"]),
                    ("unimplemented", row["unimplemented"]),
                )
                if runner in holder
            ]
            if len(states) != 1:
                errors.append(
                    "%s/%s: stated %d times, want exactly 1"
                    % (marker, runner, len(states))
                )
                continue
            seen.add(runner)
            evidence = emits(runners[runner]["source"], marker)
            if states[0] == "evaluated":
                if evidence is None:
                    errors.append("%s/%s: claims evaluated but emits no marker" % (marker, runner))
                elif row["evaluated"][runner] != evidence:
                    errors.append(
                        "%s/%s: evidence is %s, ledger says %s"
                        % (marker, runner, evidence, row["evaluated"][runner])
                    )
            elif evidence is not None:
                errors.append(
                    "%s/%s: declared %s but %s emits it"
                    % (marker, runner, states[0], evidence)
                )
            if states[0] == "unavailable":
                key = row["unavailable"][runner]
                if key not in reasons:
                    errors.append("%s/%s: unknown reason id %s" % (marker, runner, key))
                used_reasons.add(key)
        stray = (set(row["evaluated"]) | set(row["unavailable"]) | set(row["unimplemented"])) - seen
        if stray:
            errors.append("%s: unknown runners %s" % (marker, ", ".join(sorted(stray))))
        for runner in row.get("notes", {}):
            if runner not in runners:
                errors.append("%s: note for unknown runner %s" % (marker, runner))
    for key in sorted(set(reasons) - used_reasons):
        errors.append("reason %s is declared but never used" % key)
    return errors


def render(ledger):
    runners = list(ledger["runners"])
    header = "| Oracle | Marker | " + " | ".join(runners) + " |"
    lines = [
        "Coverage is enforced by `validation/oracles/check.py` against"
        " `validation/oracles/coverage.json`; this table is generated from it."
        " `yes` means that runner emits the marker, `no` means the platform"
        " cannot express the oracle (the ledger carries the reason), and `todo`"
        " means it could be written and has not been.",
        "",
        header,
        "|" + "---|" * (len(runners) + 2),
    ]
    for row in ledger["oracles"]:
        cells = []
        for runner in runners:
            if runner in row["evaluated"]:
                cells.append("yes")
            elif runner in row["unavailable"]:
                cells.append("no")
            else:
                cells.append("todo")
        lines.append(
            "| `%s` | `%s` | %s |" % (row["oracle"], row["marker"], " | ".join(cells))
        )
    counts = {state: 0 for state in STATES}
    for row in ledger["oracles"]:
        counts["evaluated"] += len(row["evaluated"])
        counts["unavailable"] += len(row["unavailable"])
        counts["unimplemented"] += len(row["unimplemented"])
    lines += [
        "",
        "%d of %d pairs are evaluated, %d cannot be expressed by the platform,"
        " and %d are unwritten."
        % (
            counts["evaluated"],
            sum(counts.values()),
            counts["unavailable"],
            counts["unimplemented"],
        ),
    ]
    return "\n".join(lines)


def splice(text, block):
    start = text.index(BEGIN) + len(BEGIN)
    end = text.index(END)
    return text[:start] + "\n\n" + block + "\n\n" + text[end:]


def main():
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check-generated", action="store_true")
    args = parser.parse_args()

    ledger = load(LEDGER)
    errors = validate(ledger)
    if errors:
        for error in errors:
            print("FAIL %s" % error)
        return 1

    doc = read(DOC)
    if BEGIN not in doc or END not in doc:
        print("FAIL docs/oracles.md has no generated:oracle-coverage block")
        return 1
    expected = splice(doc, render(ledger))
    if args.write:
        with open(DOC, "w", encoding="utf8") as handle:
            handle.write(expected)
        print("wrote the generated coverage block into docs/oracles.md")
        return 0
    if args.check_generated and doc != expected:
        print("FAIL docs/oracles.md is stale; run check.py --write")
        return 1
    print(
        "ok: %d oracle markers x %d runners, every pair stated"
        % (len(ledger["oracles"]), len(ledger["runners"]))
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failure as failure:
        print("FAIL %s" % failure)
        sys.exit(1)
