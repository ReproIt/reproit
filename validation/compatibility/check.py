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
    policy = validate_policy(support["policy"], set(targets))

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
        validate_target(target_id, target, known_gates, policy, jobs)
        require(target["displayName"] not in display_names,
                f"{target_id}: displayName is duplicated")
        display_names.add(target["displayName"])
    return policy


def status_document(support: dict) -> dict:
    known_gates = load_json(GATES_PATH)["gates"]
    targets = []
    for target_id in sorted(support["targets"]):
        target = support["targets"][target_id]
        promotion = target["promotion"]
        production_to_local = promotion["productionToLocal"]
        targets.append(
            {
                "id": target_id,
                "displayName": target["displayName"],
                "family": target["family"],
                "maturity": target["maturity"],
                "scope": target["scope"],
                "nativeGates": target["ownedGates"],
                "promotionStandard": promotion["standard"],
                "fieldBenchmark": promotion["fieldBenchmark"],
                "qualifications": {
                    slot: promotion[slot]["kind"] for slot in QUALIFICATION_SLOTS
                },
                "bounds": platform_bounds(target, known_gates),
                "productionToLocal": production_to_local["level"],
                "productionToLocalEvidence": production_to_local["evidence"],
                "promotionBlockers": [
                    {"code": blocker["code"], "detail": blocker["detail"]}
                    for blocker in promotion["blockers"]
                ],
            }
        )
    counts = {
        maturity: sum(target["maturity"] == maturity for target in targets)
        for maturity in ("stable", "preview", "experimental")
    }
    qualification_counts = {
        state: sum(
            target["productionToLocal"] == state for target in targets
        )
        for state in sorted(PRODUCTION_TO_LOCAL)
    }
    return {
        "schemaVersion": 3,
        "policy": support["policy"],
        "counts": counts,
        "productionToLocalCounts": qualification_counts,
        "targets": targets,
    }


def markdown_status(status: dict) -> str:
    lines = [
        "# Compatibility qualification status",
        "",
        "Generated from `validation/support-manifest.json`. Do not edit by hand.",
        "",
    ]
    for target in status["targets"]:
        bounds = target["bounds"]
        lines.extend(
            [
                f"## {target['displayName']}",
                "",
                f"- Maturity: {target['maturity'].title()}",
                *textwrap.wrap(
                    f"- Scope: {target['scope']}",
                    width=100,
                    subsequent_indent="  ",
                ),
                f"- Promotion standard: {target['promotionStandard']}",
                f"- Native gates: {', '.join(target['nativeGates']) or 'missing'}",
                f"- Field benchmark: {target['fieldBenchmark'] or 'incomplete'}",
                f"- Production-to-local: {target['productionToLocal']}",
                (
                    "- Production-to-local evidence: "
                    f"{target['productionToLocalEvidence'] or 'none'}"
                ),
                f"- Operating systems: {', '.join(bounds['os'])}",
                f"- Architectures: {', '.join(bounds['arch'])}",
                f"- Runtimes: {', '.join(bounds['runtime'])}",
                f"- Frameworks: {', '.join(bounds['framework'])}",
                "- Qualifications:",
            ]
        )
        for slot in QUALIFICATION_SLOTS:
            lines.append(f"  - {slot}: {target['qualifications'][slot]}")
        lines.append("- Promotion blockers:")
        blockers = target["promotionBlockers"]
        if not blockers:
            lines.append("  - None")
        for blocker in blockers:
            lines.extend(
                textwrap.wrap(
                    f"  - [{blocker['code']}] {blocker['detail']}",
                    width=100,
                    subsequent_indent="    ",
                )
            )
        lines.append("")
    lines.extend(
        [
            "A Stable target requires exact-commit native evidence, two independent",
            "affected-versus-fixed applications, three clean affected reproductions,",
            "three reached-observation fixed controls, exact identity preservation,",
            "verified minimization, and neighboring legal behavior. Targets on the",
            "schema-3 standard additionally require a clean corpus, an adversarial",
            "corpus, a clean package installation, and a confirmed manual review.",
            "",
        ]
    )
    return "\n".join(lines)


def shell_argv(argv: list[str]) -> list[str]:
    """Render checked-in gate argv as a copyable, width-bounded shell command."""
    tokens = [
        value if re.fullmatch(r"[A-Za-z0-9_./:-]+", value) else json.dumps(value)
        for value in argv
    ]
    lines: list[str] = []
    current = ""
    for token in tokens:
        candidate = f"{current} {token}".strip()
        if current and len(candidate) > 88:
            lines.append(f"{current} \\")
            current = f"  {token}"
        else:
            current = candidate
    if current:
        lines.append(current)
    return lines


def stability_plan(status: dict, gates: dict) -> str:
    preview = [target for target in status["targets"] if target["maturity"] != "stable"]
    stable = [target for target in status["targets"] if target["maturity"] == "stable"]
    targets_by_id = {target["id"]: target for target in status["targets"]}
    lane_target_list = [
        target_id
        for lane in STABILITY_LANES
        for target_id in lane["targets"]
    ]
    lane_target_ids = set(lane_target_list)
    require(
        len(lane_target_list) == len(lane_target_ids),
        "stability execution lanes contain a duplicate target",
    )
    require(
        lane_target_ids == set(targets_by_id),
        "stability execution lanes must cover every target exactly once",
    )
    total = len(status["targets"])
    lines = [
        "# All-target stability plan",
        "",
        "Generated from `validation/support-manifest.json` and",
        "`validation/backends/evidence.json`. Do not edit by hand.",
        "",
        "## Program end state",
        "",
        f"This program is complete only when all {total} atomic targets satisfy both axes:",
        "",
        f"- {total} of {total} targets are Stable under schema-3.",
        f"- {total} of {total} targets are `IndependentQualified` for production-to-local.",
        "- No target has a typed promotion blocker or a missing qualification slot.",
        "- The grandfathered schema-2 set is empty.",
        "- Every native result, field campaign, and production chain names one exact commit.",
        "- Generated compatibility surfaces agree with the canonical manifest.",
        "",
        "Stable and production-to-local remain independent claims. Reaching Stable does not",
        "silently grant production qualification, and a fixture replay never counts as an",
        "independent application replay.",
        "",
        "## Stable completion contract",
        "",
        "A target becomes Stable only when the manifest validator can prove all of these:",
        "",
        "1. Every owned native gate is required CI and passes at the exact CLI commit.",
        "2. Two independent affected-versus-fixed application campaigns are retained.",
        "3. Each application has three clean affected reproductions with one exact identity.",
        "4. Each application has three reached-observation fixed controls.",
        "5. Minimization and neighboring legal behavior are verified.",
        "6. Clean and adversarial corpora, package installation, and manual review are retained.",
        "7. Every typed blocker is removed because evidence closes it, never by prose alone.",
        "",
        "## Qualification evidence contract",
        "",
        "Before changing any `productionToLocal` value, extend the manifest schema so the value",
        "is derived from a retained evidence record instead of a manually editable string.",
        "Each record must bind the target id, qualification level, exact CLI and SDK commits,",
        "application revision, origin type, Cloud occurrence identity, trusted local provider,",
        "input and artifact hashes, replay command, behavioral assertion, reset, and cleanup.",
        "",
        "The qualification levels have distinct gates:",
        "",
        "1. `FixtureQualified`: run a disposable SDK-to-Cloud-to-trusted-local chain from a",
        "   controlled fixture, retain every stage, assert exact local behavior, and clean up.",
        "2. `IndependentQualified`: repeat the complete chain from a real independent affected",
        "   application occurrence for the exact target. A renamed or modified built-in fixture",
        "   does not qualify.",
        "",
        "`validation/cloud/run-production-loop.sh` is the web fixture reference harness. It can",
        "prove `FixtureQualified` only for the target it actually exercises. Add a bounded",
        "target adapter, or an equally strict target-specific harness, before using the workflow",
        "for mobile, desktop, terminal, or backend targets.",
        "",
        "## Shared prerequisites and dependency order",
        "",
        "1. Freeze one reviewable candidate commit. Update Cloud, SDK, package, and deployment",
        "   pins to that exact commit before collecting promotion evidence.",
        "2. Add evidence-backed qualification fields and validators before changing any",
        "   `productionToLocal` state.",
        "3. Close runner gaps per lane. Every runner must prove process ownership, readiness,",
        "   reset, containment, bounded work, artifact retention, and cleanup on every exit.",
        "4. Ratchet the existing Stable targets from schema-2 to schema-3. Keep them Stable only",
        "   if the exact-commit gates and new corpus evidence pass.",
        "5. Run owned native gates and field campaigns by lane, but validate and promote each",
        "   target independently. Never use one framework's evidence for a neighboring target.",
        "6. For every Stable target, retain a target-specific fixture production chain and advance",
        "   only that target to `FixtureQualified`.",
        "7. For every fixture-qualified target, retain a real independent application chain and",
        "   advance only that target to `IndependentQualified`.",
        "8. Regenerate all public surfaces and run the final all-target audit.",
        "",
        "## Execution lanes",
        "",
    ]
    for lane in STABILITY_LANES:
        target_names = [
            targets_by_id[target_id]["displayName"]
            for target_id in lane["targets"]
        ]
        lines.extend(
            [
                f"### {lane['name']}",
                "",
                *textwrap.wrap(
                    f"- Route: {lane['route']}",
                    width=100,
                    subsequent_indent="  ",
                ),
                *textwrap.wrap(
                    f"- Targets: {', '.join(target_names)}",
                    width=100,
                    subsequent_indent="  ",
                ),
                *textwrap.wrap(
                    f"- Lane prerequisite: {lane['prerequisite']}",
                    width=100,
                    subsequent_indent="  ",
                ),
                "- Exit gate: every target-specific native command passes at the candidate",
                "  commit, and retained reset and cleanup evidence validates.",
                "",
            ]
        )
    lines.extend(
        [
            "## Existing Stable ratchet",
            "",
            "These targets are not finished merely because they are already Stable. They must pass",
            "the current exact-commit gates, move to schema-3, and complete both production",
            "qualification levels.",
            "",
        ]
    )
    for target in stable:
        missing = [
            name for name, kind in target["qualifications"].items() if kind == "missing"
        ]
        lines.append(
            f"- {target['displayName']}: "
            + (
                f"add {', '.join(missing)} and move to schema-3"
                if missing
                else "already satisfies every recorded qualification slot"
            )
        )
    lines.extend(
        [
            "",
            "## Preview target worklists",
            "",
            "Execute these worklists in lane order. A target leaves this section only when its",
            "complete target-specific record validates.",
            "",
        ]
    )
    for target in preview:
        lines.extend(
            [
                f"### {target['displayName']}",
                "",
                f"- Target id: `{target['id']}`",
                f"- Current maturity: {target['maturity'].title()}",
                f"- Environment: {', '.join(target['bounds']['os'])}; "
                f"{', '.join(target['bounds']['arch'])}",
                f"- Runtime bound: {', '.join(target['bounds']['runtime'])}",
                f"- Framework bound: {', '.join(target['bounds']['framework'])}",
                "- Native gates:",
            ]
        )
        for gate_id in target["nativeGates"]:
            gate = gates[gate_id]
            automation = gate["automation"]
            route = automation.get("route")
            suffix = f"; route: {route}" if route else ""
            lines.extend(
                textwrap.wrap(
                    f"  - `{gate_id}`: {automation['mode']} in "
                    f"{automation['workflow']} job `{automation['job']}`{suffix}",
                    width=100,
                    subsequent_indent="    ",
                )
            )
            lines.extend(
                [
                    "    ```sh",
                    *(f"    {line}" for line in shell_argv(gate["command"])),
                    "    ```",
                ]
            )
        lines.append(
            f"- Field benchmark: `{target['fieldBenchmark']}`"
            if target["fieldBenchmark"]
            else f"- Field benchmark to create: `validation/field/{target['id']}.json`"
        )
        lines.append("- Open blockers:")
        for blocker in target["promotionBlockers"]:
            lines.extend(
                textwrap.wrap(
                    f"  - [{blocker['code']}] {blocker['detail']}",
                    width=100,
                    subsequent_indent="    ",
                )
            )
        lines.extend(
            [
                "- Promotion gate:",
                f"  - Set `{target['id']}.maturity` to `stable` only after the benchmark,",
                "    qualification slots, required-CI gates, and blockers validate together.",
                "- Qualification dependency:",
                "  - After Stable, run the target-specific fixture chain and then a distinct",
                "    independent application chain. Retain and validate both records.",
                "",
            ]
        )
    lines.extend(
        [
            "## All-target production-to-local qualification",
            "",
            "The following checklist covers every target, including the targets that were Stable",
            "when this plan was generated. Each row is complete only at `IndependentQualified`.",
            "",
            "| Target | Current maturity | Current qualification | Required end state |",
            "|---|---|---|---|",
        ]
    )
    for target in status["targets"]:
        lines.append(
            f"| `{target['id']}` | {target['maturity'].title()} | "
            f"{target['productionToLocal']} | Stable + `IndependentQualified` |"
        )
    lines.extend(
        [
            "",
            "For each row, use this atomic sequence:",
            "",
            "1. Confirm the target's schema-3 Stable record at the candidate commit.",
            "2. Run and retain the target-specific controlled fixture chain.",
            "3. Validate the record and advance only that target to `FixtureQualified`.",
            "4. Capture a real occurrence from an independent application at a pinned revision.",
            "5. Ingest it through the SDK into a disposable isolated Cloud project.",
            "6. Replay it locally with the declared trusted provider and assert exact behavior.",
            "7. Retain immutable stage hashes plus reset and cleanup proof.",
            "8. Validate the independent record and advance only that target to",
            "   `IndependentQualified`.",
            "",
            "## Final audit",
            "",
            f"- Assert Stable count: {total}.",
            f"- Assert `IndependentQualified` count: {total}.",
            "- Assert Preview and Experimental counts: zero.",
            "- Assert blocker count, missing qualification slots, and schema-2 targets: zero.",
            "- Re-run every required-CI and native gate at the same exact commit.",
            "- Review retained evidence for target identity, independent origin, hashes, reset,",
            "  containment, and cleanup before publishing the generated claims.",
            "",
            "## Required validation",
            "",
            "```sh",
            "python3 validation/compatibility/check.py --write",
            "python3 validation/compatibility/check.py --check-generated",
            "cargo fmt --all --check",
            "cargo clippy --workspace --all-targets -- -D warnings",
            "cargo test --workspace --locked",
            "```",
            "",
            "Then run every target's native command above on its declared environment and retain",
            "the exact-commit evidence required by `validation/release/check-native-evidence.py`.",
            "",
        ]
    )
    return "\n".join(lines)


def readme_platforms(status: dict) -> str:
    return "\n".join(
        f"- {target['displayName']}"
        for target in status["targets"]
    )


def compatibility_section(status: dict) -> str:
    counts = status["counts"]
    lines = [
        f"Stable atomic targets: {counts['stable']}. "
        f"Preview: {counts['preview']}. "
        f"Experimental: {counts['experimental']}.",
        "",
        "| Target | Maturity | Standard | OS | Architectures | Blockers |",
        "|---|---|---|---|---|---|",
    ]
    for target in status["targets"]:
        bounds = target["bounds"]
        lines.append(
            f"| {target['displayName']} | {target['maturity'].title()} | "
            f"{target['promotionStandard']} | {', '.join(bounds['os'])} | "
            f"{', '.join(bounds['arch'])} | {len(target['promotionBlockers'])} |"
        )
    lines.extend(
        [
            "",
            "Every blocker, with its typed code and exact detail, is listed in",
            "[the generated status](../validation/compatibility/STATUS.md).",
        ]
    )
    return "\n".join(lines)


def support_claim(status: dict) -> str:
    stable = [t["displayName"] for t in status["targets"] if t["maturity"] == "stable"]
    preview = [t["displayName"] for t in status["targets"] if t["maturity"] == "preview"]
    qualified = [
        t["displayName"]
        for t in status["targets"]
        if t["productionToLocal"] != "Unqualified"
    ]
    lines = [
        f"Stable ({len(stable)}): " + (", ".join(stable) or "none") + ".",
        "",
        f"Preview ({len(preview)}): " + (", ".join(preview) or "none") + ".",
        "",
        "Production-to-local qualified: " + (", ".join(qualified) or "none") + ".",
        "",
        "Stable is an atomic compatibility claim. It does not by itself claim",
        "that every production occurrence on that target reproduces locally;",
        "that is the separate production-to-local qualification above.",
    ]
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
        "STABILITY_PLAN.md": (
            STABILITY_PLAN_PATH,
            stability_plan(status, gate_document["gates"]),
        ),
        "README.md": (README_PATH, splice(README_PATH, "platforms", readme_platforms(status))),
        "docs/compatibility.md": (
            COMPATIBILITY_DOC_PATH,
            splice(COMPATIBILITY_DOC_PATH, "promotion-state", compatibility_section(status)),
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
