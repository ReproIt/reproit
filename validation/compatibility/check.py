#!/usr/bin/env python3
"""Render the validated compatibility contract into public status surfaces."""

from compatibility_contract import *  # noqa: F403
def validate_support(support: object, gates: object) -> dict:
    require(isinstance(support, dict), "support manifest root must be an object")
    require(set(support) == {"schema", "policy", "comment", "targets"},
            "support manifest keys do not match schema 3")
    require(support["schema"] == 3, "unsupported support manifest schema")
    require(isinstance(support["comment"], str) and support["comment"],
            "support manifest comment must be non-empty")
    targets = support["targets"]
    require(isinstance(targets, dict) and targets, "support manifest has no targets")
    policy = validate_policy(support["policy"])

    require(isinstance(gates, dict) and gates.get("schema") == 2,
            "native gate manifest schema is unsupported")
    known_gates = gates.get("gates")
    require(isinstance(known_gates, dict) and known_gates,
            "native gate manifest has no gates")
    jobs = workflow_steps()
    display_names: set[str] = set()
    for target_id in sorted(targets):
        target = targets[target_id]
        require(target_id.replace("-", "").isalnum() and target_id == target_id.lower(),
                f"{target_id}: target id is invalid")
        validate_target(target_id, target, known_gates, jobs)
        require(target["displayName"] not in display_names,
                f"{target_id}: displayName is duplicated")
        display_names.add(target["displayName"])
    return policy


def status_document(support: dict) -> dict:
    known_gates = load_json(GATES_PATH)["gates"]
    targets = []
    for target_id in sorted(support["targets"]):
        target = support["targets"][target_id]
        evidence = target["evidence"]
        targets.append(
            {
                "id": target_id,
                "displayName": target["displayName"],
                "family": target["family"],
                "scope": target["scope"],
                "nativeGates": target["ownedGates"],
                "releaseGates": target["releaseGates"],
                "fieldBenchmark": evidence["fieldBenchmark"],
                "evidenceGaps": evidence_gaps(evidence),
                "evidence": {
                    slot: evidence[slot]["kind"] for slot in EVIDENCE_SLOTS
                },
                "bounds": platform_bounds(target, known_gates),
            }
        )
    return {
        "schemaVersion": 3,
        "policy": support["policy"],
        "targets": targets,
    }


def evidence_gaps(evidence: dict) -> list[str]:
    """Recorded facts about absent evidence. Facts, never a status label:
    every declared target is supported, and a gap names work, not a tier."""
    gaps = [
        f"{slot} evidence is missing"
        for slot in EVIDENCE_SLOTS
        if evidence[slot]["kind"] == "missing"
    ]
    benchmark_path = evidence["fieldBenchmark"]
    if benchmark_path is None:
        gaps.append("independent field benchmark is missing")
    else:
        benchmark = load_json(ROOT / benchmark_path)
        if benchmark.get("status") != "complete":
            gaps.append("independent field benchmark is pending")
    return gaps


def markdown_status(status: dict) -> str:
    lines = [
        "# Supported platform targets",
        "",
        "Generated from `validation/support-manifest.json`. Do not edit by hand.",
        "",
        f"Reproit declares {len(status['targets'])} atomic targets.",
        "",
    ]
    for target in status["targets"]:
        bounds = target["bounds"]
        lines.extend(
            [
                f"## {target['displayName']}",
                "",
                f"- Target id: `{target['id']}`",
                f"- Family: {target['family']}",
                *textwrap.wrap(
                    f"- Scope: {target['scope']}",
                    width=100,
                    subsequent_indent="  ",
                ),
                f"- Native gates: {', '.join(target['nativeGates'])}",
                "- Release evidence directories: "
                + ", ".join(
                    f"{gate_id} in {directory}"
                    for gate_id, directory in sorted(target["releaseGates"].items())
                ),
                f"- Field benchmark: {target['fieldBenchmark'] or 'none recorded'}",
                f"- Platforms: {', '.join(bounds['platforms'])}",
                f"- Runtimes: {', '.join(bounds['runtime'])}",
                f"- Frameworks: {', '.join(bounds['framework'])}",
                "- Evidence slots:",
            ]
        )
        for gap in target["evidenceGaps"]:
            lines.append(f"- Evidence gap: {gap}")
        for slot in EVIDENCE_SLOTS:
            lines.append(f"  - {slot}: {target['evidence'][slot]}")
        lines.append("")
    return "\n".join(lines)


README_FAMILY_ORDER = (
    "backend",
    "web",
    "native-mobile",
    "flutter",
    "desktop-webview",
    "desktop",
    "tui",
)


def readme_platforms(status: dict) -> str:
    """Render one line per framework with the platforms it reaches.

    Generated from the canonical record, and shaped by what a developer builds
    with rather than by how CI happens to be sharded: a framework that runs on
    two platforms is one entry naming both, not two entries named after runners.
    """
    platforms: dict[str, list[str]] = {}
    first_family: dict[str, str] = {}
    for target in status["targets"]:
        bounds = target["bounds"]
        for framework in bounds["framework"]:
            first_family.setdefault(framework, target["family"])
            for platform in bounds["platforms"]:
                reach = platforms.setdefault(framework, [])
                if platform not in reach:
                    reach.append(platform)
    unordered = sorted(set(first_family.values()) - set(README_FAMILY_ORDER))
    lines = []
    for family in (*README_FAMILY_ORDER, *unordered):
        for framework, reach in platforms.items():
            if first_family[framework] == family:
                lines.append(f"- **{framework}**: {', '.join(reach)}")
    return "\n".join(lines)




def target_section(status: dict) -> str:
    lines = [
        f"Declared atomic targets: {len(status['targets'])}.",
        "",
        "| Target | Framework | Platforms | Driving runtime |",
        "|---|---|---|---|",
    ]
    for target in status["targets"]:
        bounds = target["bounds"]
        lines.append(
            f"| {target['displayName']} | "
            f"{', '.join(bounds['framework'])} | "
            f"{', '.join(bounds['platforms'])} | {', '.join(bounds['runtime'])} |"
        )
    lines.extend(
        [
            "",
            "Every target's runtime and framework bounds, release evidence",
            "directories, and retained evidence slots are listed in",
            "[the generated status](../validation/compatibility/STATUS.md).",
        ]
    )
    return "\n".join(lines)


def support_claim(status: dict) -> str:
    names = [target["displayName"] for target in status["targets"]]
    lines = [f"Reproit supports {len(names)} atomic platform targets:"]
    lines.extend(f"- {name}" for name in names)
    lines.extend(
        [
            "",
            "Every declared target has gates for its native fixtures.",
            "The 1.0 support claim covers every declared target.",
            "The generated status records each target's retained evidence.",
        ]
    )
    return "\n".join(lines)


def splice(path: Path, name: str, body: str) -> str:
    """Replace the generated block named `name` inside `path`."""
    begin = BEGIN.format(name=name)
    end = END.format(name=name)
    text = path.read_text(encoding="utf-8")
    start = text.find(begin)
    stop = text.find(end)
    require(start != -1 and stop != -1 and start < stop,
            f"{path.relative_to(ROOT)} has no generated block named {name}")
    return f"{text[:start]}{begin}\n\n{body}\n\n{text[stop:]}"


def render() -> dict[str, tuple[Path, str]]:
    support = load_json(SUPPORT_PATH)
    gate_document = load_json(GATES_PATH)
    validate_support(support, gate_document)
    status = status_document(support)
    return {
        "status.json": (STATUS_PATH, json.dumps(status, indent=2) + "\n"),
        "STATUS.md": (MARKDOWN_PATH, markdown_status(status)),
        "README.md": (README_PATH, splice(README_PATH, "platforms", readme_platforms(status))),
        "docs/compatibility.md": (
            COMPATIBILITY_DOC_PATH,
            splice(COMPATIBILITY_DOC_PATH, "targets", target_section(status)),
        ),
        "SUPPORT.md": (
            SUPPORT_DOC_PATH,
            splice(SUPPORT_DOC_PATH, "support-claim", support_claim(status)),
        ),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check-generated", action="store_true")
    args = parser.parse_args()
    rendered = render()
    if args.write:
        for path, body in rendered.values():
            path.write_text(body, encoding="utf-8")
    elif args.check_generated:
        for name, (path, body) in rendered.items():
            require(path.read_text(encoding="utf-8") == body,
                    f"{name} is stale; run validation/compatibility/check.py --write")
    else:
        print(rendered["status.json"][1], end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
