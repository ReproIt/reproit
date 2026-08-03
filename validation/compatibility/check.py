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


def markdown_status(status: dict) -> str:
    lines = [
        "# Supported platform targets",
        "",
        "Generated from `validation/support-manifest.json`. Do not edit by hand.",
        "",
        f"Reproit supports {len(status['targets'])} atomic targets.",
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
                f"- Operating systems: {', '.join(bounds['os'])}",
                f"- Architectures: {', '.join(bounds['arch'])}",
                f"- Runtimes: {', '.join(bounds['runtime'])}",
                f"- Frameworks: {', '.join(bounds['framework'])}",
                "- Evidence slots:",
            ]
        )
        for slot in EVIDENCE_SLOTS:
            lines.append(f"  - {slot}: {target['evidence'][slot]}")
        lines.append("")
    return "\n".join(lines)


README_FAMILY_ORDER = (
    "backend",
    "web",
    "desktop-webview",
    "desktop",
    "native-mobile",
    "flutter",
    "tui",
)

README_FAMILY_TITLES = {
    "backend": "Backend services",
    "web": "Web",
    "desktop-webview": "Desktop webview",
    "desktop": "Desktop native",
    "native-mobile": "Mobile",
    "flutter": "Flutter",
    "tui": "Terminal",
}


def readme_platforms(status: dict) -> str:
    """Render the README list grouped by family, backend first.

    The grouping is generated, not hand-written prose, so the emphasis cannot
    drift from the canonical record. This list answers "does Reproit reach my
    stack"; the per-target bounds and gates live in `docs/compatibility.md`.
    """
    families: dict[str, list[dict]] = {}
    for target in status["targets"]:
        families.setdefault(target["family"], []).append(target)
    unordered = sorted(set(families) - set(README_FAMILY_ORDER))
    blocks = []
    for family in (*README_FAMILY_ORDER, *unordered):
        targets = families.get(family)
        if not targets:
            continue
        names = "\n".join(f"- {target['displayName']}" for target in targets)
        blocks.append(f"**{README_FAMILY_TITLES.get(family, family)}**\n\n{names}")
    return "\n\n".join(blocks)


def target_section(status: dict) -> str:
    lines = [
        f"Supported atomic targets: {len(status['targets'])}.",
        "",
        "| Target | Family | Native gates | OS | Architectures |",
        "|---|---|---|---|---|",
    ]
    for target in status["targets"]:
        bounds = target["bounds"]
        lines.append(
            f"| {target['displayName']} | {target['family']} | "
            f"{', '.join(target['nativeGates'])} | {', '.join(bounds['os'])} | "
            f"{', '.join(bounds['arch'])} |"
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
    names = ", ".join(target["displayName"] for target in status["targets"])
    return "\n".join(
        [
            f"Reproit supports {len(status['targets'])} atomic platform targets: "
            f"{names}.",
            "",
            "Each one is gated by the native fixtures it owns, and each one is",
            "covered by the 1.x compatibility promise.",
        ]
    )


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
