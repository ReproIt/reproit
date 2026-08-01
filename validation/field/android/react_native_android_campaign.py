#!/usr/bin/env python3
"""Exercise independent React Native Android defects on reset x86_64 AVDs."""

from __future__ import annotations

import argparse
import json
import re
import time
from pathlib import Path
from typing import Callable

from nextplayer_permission_loop import (
    AppiumServer,
    AppiumSession,
    Device,
    run_with_reset,
    sha256,
)
from react_native_android_ui import (
    grant_permission,
    install,
    long_press_text,
    nodes,
    press_back,
    retain_observation,
    reveal_music_tab,
    swipe_left,
    tap_text,
    wait_source,
)
from react_native_android_runtime import (
    APPIUM_VERSION,
    AVD_SYSTEM_IMAGE,
    AVD_SYSTEM_IMAGE_SHA256,
    EMULATOR_SHA256,
    EMULATOR_VERSION,
    UIAUTOMATOR2_VERSION,
    cleanup_audit,
    enforce_offline,
    record_owned_process,
    validate_runtime,
)

ELEMENT_TEXT_KEYS = ("text", "content-desc")
JOPLIN_PACKAGE = "net.cozic.joplin"
JOPLIN_ACTIVITY = ".MainActivity"
JOPLIN_REPOSITORY = "https://github.com/laurent22/joplin"
JOPLIN_ISSUE = "https://github.com/laurent22/joplin/issues/15004"
JOPLIN_AFFECTED_REVISION = "de6378473fa261e495b4709672471613235b493a"
JOPLIN_FIXED_REVISION = "623da377db98dbc8576651aa066ef4000fbf2116"
JOPLIN_IDENTITY = (
    "react-native-navigation:hardware-back-disabled-after-deleted-notebook"
)
MUSIC_PACKAGE = "com.cyanchill.missingcore.music"
MUSIC_ACTIVITY = ".MainActivity"
MUSIC_REPOSITORY = "https://github.com/MissingCore/Music"
MUSIC_ISSUE = "https://github.com/MissingCore/Music/issues/220"
MUSIC_AFFECTED_REVISION = "cdd2305aa0ae3bb5dcefe0691090a1d57cf53cb3"
MUSIC_FIXED_REVISION = "5c86ff15ee99ac8f77abc19b9e58b98a705a9951"
MUSIC_IDENTITY = "react-native-state:album-split-by-inconsistent-release-year"
# The sidebar notebook row, addressed by the one string only it carries. The
# note list header repeats the title with the emoji and a space and carries no
# content-desc at all, so the comma is what separates them. UiAutomator2
# appends a nesting suffix such as "  (level 1)" once the row has been
# re-rendered, so this is a prefix rather than a whole value, and it is matched
# against the PARSED tree: the raw dump escapes the emoji as a numeric entity.
SIDEBAR_NOTEBOOK = "\N{WAVING HAND SIGN}, Welcome!"
# Setting.ts asks for 'Synchronisation' and the shipped en_US catalogue renders
# it 'Synchronization'. Accept the label the device actually draws.
SYNC_SECTION = ("Synchronisation", "Synchronization")
MUSIC_FIXTURE_NAMES = {
    "control-alpha.mp3",
    "control-beta.mp3",
    "field-none.mp3",
    "field-year.mp3",
}


def music_fixture_hashes(fixture_dir: Path) -> dict[str, str]:
    fixtures = {}
    for fixture in sorted(fixture_dir.glob("*.mp3")):
        fixtures[fixture.name] = f"sha256:{sha256(fixture)}"
    if set(fixtures) != MUSIC_FIXTURE_NAMES:
        raise RuntimeError(f"unexpected Music fixture set: {sorted(fixtures)}")
    return fixtures


def select_music_fixtures(
    fixture_dir: Path,
    fixture_names: set[str],
) -> list[Path]:
    unexpected = fixture_names - MUSIC_FIXTURE_NAMES
    if unexpected:
        raise RuntimeError(f"unexpected requested Music fixtures: {sorted(unexpected)}")
    fixtures = [fixture_dir / name for name in sorted(fixture_names)]
    missing = [str(fixture) for fixture in fixtures if not fixture.is_file()]
    if missing:
        raise RuntimeError(f"missing requested Music fixtures: {missing}")
    return fixtures


def seed_music(device: Device, fixtures: list[Path]) -> str:
    """Push the fixture set and make MediaStore index it before the app scans.

    ACTION_MEDIA_SCANNER_SCAN_FILE is a protected broadcast that MediaProvider
    stopped honouring long before API 36, so it silently indexes nothing: the
    first executed run reached the Music home screen with the track count still
    reading zero. MediaStore.scanFile and scanVolume are reachable from the
    shell as provider calls, and the returned row count is the proof the volume
    really was indexed rather than the request merely accepted.
    """
    remote_dir = "/sdcard/Music/ReproitField"
    device.adb_run("shell", "mkdir", "-p", remote_dir)
    for fixture in fixtures:
        remote = f"{remote_dir}/{fixture.name}"
        device.adb_run("push", str(fixture), remote, timeout=120)
        device.adb_run(
            "shell",
            "content",
            "call",
            "--uri",
            "content://media",
            "--method",
            "scan_file",
            "--arg",
            remote,
            check=False,
            timeout=120,
        )
    device.adb_run(
        "shell",
        "content",
        "call",
        "--uri",
        "content://media",
        "--method",
        "scan_volume",
        "--arg",
        "external_primary",
        check=False,
        timeout=300,
    )
    indexed = device.adb_run(
        "shell",
        "content",
        "query",
        "--uri",
        "content://media/external/audio/media",
        "--projection",
        "_display_name",
        capture=True,
        check=False,
        timeout=120,
    )
    found = {
        fixture.name for fixture in fixtures if fixture.name in indexed
    }
    if found != {fixture.name for fixture in fixtures}:
        raise RuntimeError(
            "MediaStore did not index the whole fixture set: "
            f"{sorted(found)} of {sorted(f.name for f in fixtures)}"
        )
    return indexed


def visible_media_cards(source: str) -> list[dict[str, object]]:
    cards = []
    for node in nodes(source):
        if node.attrib.get("clickable") != "true":
            continue
        texts = sorted(
            {
                descendant.attrib.get("text", "")
                for descendant in node.iter()
                if descendant.attrib.get("text")
            }
        )
        if texts:
            cards.append(
                {
                    "bounds": node.attrib.get("bounds", ""),
                    "texts": texts,
                }
            )
    return cards


def music_album_signatures(session: AppiumSession) -> tuple[list[dict], str]:
    signatures = []
    final_source = ""
    for index in range(7):
        final_source = session.source()
        text_values = sorted(
            {
                node.attrib.get("text", "")
                for node in nodes(final_source)
                if node.attrib.get("text")
            }
        )
        signatures.append(
            {
                "step": index,
                "texts": text_values,
                "clickableCards": visible_media_cards(final_source),
            }
        )
        swipe_left(session)
        time.sleep(1)
    return signatures, final_source


def music_signature_flags(signatures: list[dict]) -> dict:
    snapshots = [set(entry["texts"]) for entry in signatures]
    card_texts = [
        set(card["texts"])
        for entry in signatures
        for card in entry["clickableCards"]
    ]
    field_cards = [card for card in card_texts if "Reproit Field Album" in card]
    with_year = any("2024" in card and "————" not in card for card in field_cards)
    without_year = any("————" in card and "2024" not in card for card in field_cards)
    field_descriptions = sorted(
        {
            description
            for card in field_cards
            for description in card
            if description in {"2024", "————"}
        }
    )
    return {
        "fieldAlbumDescriptions": field_descriptions,
        "fieldAlbumSplit": with_year and without_year,
        "controlAlpha": any("Reproit Control Alpha" in values for values in snapshots),
        "controlBeta": any("Reproit Control Beta" in values for values in snapshots),
    }


def music_fixture_names(observation_mode: str) -> set[str]:
    fixture_names = {
        "benchmark": MUSIC_FIXTURE_NAMES,
        "clean-distinct-albums": {
            "control-alpha.mp3",
            "control-beta.mp3",
        },
        "adversarial-grouped-album": {
            "field-none.mp3",
            "field-year.mp3",
        },
    }.get(observation_mode)
    if fixture_names is None:
        raise RuntimeError(f"unexpected Music observation mode: {observation_mode}")
    return fixture_names


def music_observation_reached(flags: dict, observation_mode: str) -> bool:
    if observation_mode == "benchmark":
        return all(
            (
                len(flags["fieldAlbumDescriptions"]) >= 1,
                flags["controlAlpha"],
                flags["controlBeta"],
            )
        )
    if observation_mode == "clean-distinct-albums":
        return all(
            (
                not flags["fieldAlbumDescriptions"],
                flags["controlAlpha"],
                flags["controlBeta"],
            )
        )
    return all(
        (
            len(flags["fieldAlbumDescriptions"]) == 1,
            not flags["controlAlpha"],
            not flags["controlBeta"],
        )
    )


def capture_music_ui(
    device: Device,
    label: str,
    observation_mode: str,
) -> tuple[dict, dict, list[dict], dict]:
    server = AppiumServer(device.evidence, label)
    session = None
    try:
        appium_url = server.start()
        if server.process is None:
            raise RuntimeError("Music observation has no owned Appium process")
        record_owned_process("appium", label, server.process.pid)
        session = AppiumSession(
            appium_url,
            device.udid,
            MUSIC_PACKAGE,
            MUSIC_ACTIVITY,
        )
        appium = session.evidence()
        grant_permission(session, device.evidence, label)
        wait_source(
            session,
            device.evidence,
            f"{label}-home",
            lambda value: "HOME" in value and "FOLDERS" in value,
            seconds=180,
        )
        wait_source(
            session,
            device.evidence,
            f"{label}-scan",
            lambda value: 'content-desc="0, Tracks"' not in value,
            seconds=300,
        )
        source = reveal_music_tab(session, device, label, "ARTISTS")
        tap_text(session, source, "ARTISTS")
        source = wait_source(
            session,
            device.evidence,
            f"{label}-artists",
            lambda value: "Reproit Field Artist" in value,
        )
        tap_text(session, source, "Reproit Field Artist")
        expected_album = (
            "Reproit Control Alpha"
            if observation_mode == "clean-distinct-albums"
            else "Reproit Field Album"
        )
        wait_source(
            session,
            device.evidence,
            f"{label}-artist",
            lambda value: expected_album in value,
        )
        signatures, source = music_album_signatures(session)
        flags = music_signature_flags(signatures)
        retained = retain_observation(device, session, label, source)
        return appium, flags, signatures, retained
    finally:
        try:
            if session is not None:
                session.close()
        finally:
            server.stop()


def observe_music(
    device: Device,
    apk: Path,
    fixture_dir: Path,
    label: str,
    expected_identity: str | None,
    observation_mode: str = "benchmark",
) -> dict:
    started = time.monotonic()
    network = enforce_offline(device)
    if device.process is None:
        raise RuntimeError("Music observation has no owned emulator process")
    record_owned_process("emulator", label, device.process.pid)
    fixtures = select_music_fixtures(
        fixture_dir,
        music_fixture_names(observation_mode),
    )
    install(device, MUSIC_PACKAGE, apk)
    indexed = seed_music(device, fixtures)
    appium, flags, signatures, retained = capture_music_ui(
        device,
        label,
        observation_mode,
    )
    identity = MUSIC_IDENTITY if flags["fieldAlbumSplit"] else None
    observation_reached = music_observation_reached(flags, observation_mode)
    if not observation_reached:
        raise RuntimeError(f"{label} did not reach all Music observations")
    if identity != expected_identity:
        raise RuntimeError(
            f"{label} identity was {identity!r}, expected {expected_identity!r}"
        )
    return {
        "status": "reproduced" if identity else "not_reproduced",
        "identity": identity,
        "cleanLaunch": True,
        "observationReached": True,
        "exceptions": [],
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "jsHeapMiB": None,
        "albumFlags": flags,
        "albumSweep": signatures,
        "fixtureSha256": {
            fixture.name: f"sha256:{sha256(fixture)}" for fixture in fixtures
        },
        "observationMode": observation_mode,
        "mediaStoreIndexed": sorted(
            row.strip() for row in indexed.splitlines() if row.strip()
        ),
        "networkContainment": network,
        "appium": appium,
        **retained,
    }


def open_joplin_configuration(
    session: AppiumSession,
    device: Device,
    label: str,
) -> str:
    source = wait_source(
        session,
        device.evidence,
        f"{label}-notes",
        lambda value: "Sidebar" in value,
        seconds=180,
    )
    tap_text(session, source, "Sidebar")
    source = wait_source(
        session,
        device.evidence,
        f"{label}-sidebar",
        lambda value: "Configuration" in value,
    )
    tap_text(session, source, "Configuration")
    return wait_source(
        session,
        device.evidence,
        f"{label}-configuration",
        configuration_screen_shown,
    )


def welcome_notebook_present(source: str) -> bool:
    """Is the Welcome notebook still listed in the sidebar?

    Parsed rather than matched against the raw dump, because the dump escapes
    the emoji as a numeric entity, and by prefix rather than by whole value,
    because a re-rendered row gains a nesting suffix. Checking the raw string
    for one exact attribute passed the moment the suffix appeared, which let a
    run continue with the notebook still present.
    """
    return any(
        node.attrib.get("content-desc", "").startswith(SIDEBAR_NOTEBOOK)
        for node in nodes(source)
    )


def configuration_screen_shown(source: str) -> bool:
    return "Configuration" in source and any(
        label in source for label in SYNC_SECTION
    )


def delete_joplin_welcome_notebook(
    session: AppiumSession,
    device: Device,
    label: str,
) -> str:
    source = wait_source(
        session,
        device.evidence,
        f"{label}-notes",
        lambda value: "Sidebar" in value,
        seconds=180,
    )
    tap_text(session, source, "Sidebar")
    source = wait_source(
        session,
        device.evidence,
        f"{label}-sidebar",
        lambda value: welcome_notebook_present(value) and "Configuration" in value,
    )
    # The sidebar row, not the note list header. The header is a
    # non-interactive View carrying the same title with no content-desc, and it
    # precedes the sidebar in document order, so matching the bare title
    # long-pressed the header: the first executed run then walked up from it
    # looking for a clickable ancestor, found none, and landed on the boundless
    # root. The row's own Button carries the comma form and is clickable.
    long_press_text(session, source, SIDEBAR_NOTEBOOK, contains=True)
    source = wait_source(
        session,
        device.evidence,
        f"{label}-notebook-actions",
        lambda value: "Notebook: Welcome!" in value and "DELETE" in value,
    )
    # side-menu-content.tsx pushes menu items labelled Edit, Delete and Cancel,
    # and the Android AlertDialog these become renders its buttons with
    # textAllCaps, so the accessibility tree carries EDIT, DELETE and CANCEL.
    # The retained joplin-affected-1-notebook-actions dump from the run that
    # first opened this sheet reads exactly ['CANCEL', 'DELETE', 'EDIT',
    # 'Notebook: Welcome!'].
    tap_text(session, source, "DELETE")
    source = wait_source(
        session,
        device.evidence,
        f"{label}-delete-confirmation",
        lambda value: "Move notebook" in value and "OK" in value,
    )
    tap_text(session, source, "OK")
    source = wait_source(
        session,
        device.evidence,
        f"{label}-welcome-deleted",
        lambda value: "Configuration" in value
        and not welcome_notebook_present(value),
    )
    tap_text(session, source, "Configuration")
    return wait_source(
        session,
        device.evidence,
        f"{label}-configuration",
        configuration_screen_shown,
    )


def observe_joplin(
    device: Device,
    apk: Path,
    label: str,
    expected_identity: str | None,
    neighboring: bool,
) -> dict:
    started = time.monotonic()
    network = enforce_offline(device)
    if device.process is None:
        raise RuntimeError("Joplin observation has no owned emulator process")
    record_owned_process("emulator", label, device.process.pid)
    install(device, JOPLIN_PACKAGE, apk)
    server = AppiumServer(device.evidence, label)
    session = None
    try:
        appium_url = server.start()
        if server.process is None:
            raise RuntimeError("Joplin observation has no owned Appium process")
        record_owned_process("appium", label, server.process.pid)
        session = AppiumSession(
            appium_url,
            device.udid,
            JOPLIN_PACKAGE,
            JOPLIN_ACTIVITY,
        )
        appium = session.evidence()
        if neighboring:
            open_joplin_configuration(session, device, label)
        else:
            delete_joplin_welcome_notebook(session, device, label)
        press_back(session)
        time.sleep(3)
        source = session.source()
        configuration_visible = (
            configuration_screen_shown(source)
        )
        identity = JOPLIN_IDENTITY if configuration_visible else None
        observation_reached = configuration_visible or "Sidebar" in source
        retained = retain_observation(device, session, label, source)
    finally:
        try:
            if session is not None:
                session.close()
        finally:
            server.stop()
    if not observation_reached:
        raise RuntimeError(f"{label} did not reach the Joplin back observation")
    if neighboring:
        expected_identity = None
    if identity != expected_identity:
        raise RuntimeError(
            f"{label} identity was {identity!r}, expected {expected_identity!r}"
        )
    return {
        "status": "reproduced" if identity else "not_reproduced",
        "identity": identity,
        "cleanLaunch": True,
        "observationReached": True,
        "exceptions": [],
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "jsHeapMiB": None,
        "configurationVisibleAfterBack": configuration_visible,
        "networkContainment": network,
        "appium": appium,
        **retained,
    }


def validate_apk(path: Path) -> str:
    if not path.is_file():
        raise SystemExit(f"APK does not exist: {path}")
    return sha256(path)


def application_setup(
    application: str,
    device: Device,
    fixture_dir: Path | None,
) -> tuple[Callable[[Path, str, str | None], dict], str, dict]:
    if application == "music":
        assert fixture_dir is not None
        observer = lambda apk, label, identity: observe_music(
            device,
            apk,
            fixture_dir,
            label,
            identity,
        )
        return observer, MUSIC_IDENTITY, {
            "repository": MUSIC_REPOSITORY,
            "issueUrl": MUSIC_ISSUE,
            "affectedRevision": MUSIC_AFFECTED_REVISION,
            "fixedRevision": MUSIC_FIXED_REVISION,
            "package": MUSIC_PACKAGE,
            "activity": MUSIC_ACTIVITY,
        }
    observer = lambda apk, label, identity: observe_joplin(
        device,
        apk,
        label,
        identity,
        False,
    )
    return observer, JOPLIN_IDENTITY, {
        "repository": JOPLIN_REPOSITORY,
        "issueUrl": JOPLIN_ISSUE,
        "affectedRevision": JOPLIN_AFFECTED_REVISION,
        "fixedRevision": JOPLIN_FIXED_REVISION,
        "package": JOPLIN_PACKAGE,
        "activity": JOPLIN_ACTIVITY,
    }


def run_benchmark_variants(
    application: str,
    device: Device,
    observer: Callable[[Path, str, str | None], dict],
    affected_apk: Path,
    fixed_apk: Path,
    identity: str,
    runs: int,
) -> tuple[list[dict], list[dict]]:
    results: dict[str, list[dict]] = {"affected": [], "fixed": []}
    for variant, apk, expected in (
        ("affected", affected_apk, identity),
        ("fixed", fixed_apk, None),
    ):
        for index in range(1, runs + 1):
            label = f"{application}-{variant}-{index}"
            record = run_with_reset(
                device,
                label,
                lambda apk=apk, label=label, expected=expected: observer(
                    apk,
                    label,
                    expected,
                ),
            )
            record["run"] = index
            results[variant].append(record)
    return results["affected"], results["fixed"]


def run_supplemental_observations(
    application: str,
    device: Device,
    affected_apk: Path,
    fixed_apk: Path,
    fixture_dir: Path | None,
    with_corpus: bool,
) -> tuple[dict, dict]:
    neighbors = {}
    corpus = {}
    if application == "joplin":
        for variant, apk in (("affected", affected_apk), ("fixed", fixed_apk)):
            label = f"joplin-neighbor-{variant}"
            neighbors[variant] = run_with_reset(
                device,
                label,
                lambda apk=apk, label=label: observe_joplin(
                    device,
                    apk,
                    label,
                    None,
                    True,
                ),
            )
        return neighbors, corpus
    if with_corpus:
        assert fixture_dir is not None
        corpus["cleanDistinctAlbums"] = run_with_reset(
            device,
            "music-corpus-clean-distinct-albums",
            lambda: observe_music(
                device,
                affected_apk,
                fixture_dir,
                "music-corpus-clean-distinct-albums",
                None,
                "clean-distinct-albums",
            ),
        )
        corpus["adversarialGroupedAlbum"] = run_with_reset(
            device,
            "music-corpus-adversarial-grouped-album",
            lambda: observe_music(
                device,
                fixed_apk,
                fixture_dir,
                "music-corpus-adversarial-grouped-album",
                None,
                "adversarial-grouped-album",
            ),
        )
    return neighbors, corpus


def runtime_evidence(device: Device, runtime_preflight: dict) -> dict:
    return {
        "platform": "android-emulator/x86_64",
        "automation": {
            "appium": APPIUM_VERSION,
            "uiautomator2": UIAUTOMATOR2_VERSION,
        },
        "apiLevel": 36,
        "avd": device.name,
        "abi": "x86_64",
        "deviceProfile": "pixel_6",
        "systemImagePackage": AVD_SYSTEM_IMAGE,
        "systemImageFile": (
            "/android-sdk/system-images/android-36/google_apis/x86_64/system.img"
        ),
        "systemImageFileSha256": f"sha256:{AVD_SYSTEM_IMAGE_SHA256}",
        "emulatorVersion": EMULATOR_VERSION,
        "emulatorExecutable": "/android-sdk/emulator-36.2.12/emulator/emulator",
        "emulatorExecutableSha256": f"sha256:{EMULATOR_SHA256}",
        "workerImage": runtime_preflight["workerImage"],
        "preflight": runtime_preflight,
        "network": (
            "Docker network mode none plus Android airplane mode, "
            "Wi-Fi disabled, and mobile data disabled"
        ),
        "reset": "recreated AVD, -wipe-data, -no-snapshot",
    }


def campaign(
    application: str,
    device: Device,
    affected_apk: Path,
    fixed_apk: Path,
    fixture_dir: Path | None,
    runs: int,
    with_corpus: bool,
) -> dict:
    runtime_preflight = validate_runtime(device)
    observer, identity, application_identity = application_setup(
        application,
        device,
        fixture_dir,
    )
    affected, fixed = run_benchmark_variants(
        application,
        device,
        observer,
        affected_apk,
        fixed_apk,
        identity,
        runs,
    )
    neighbors, corpus = run_supplemental_observations(
        application,
        device,
        affected_apk,
        fixed_apk,
        fixture_dir,
        with_corpus,
    )
    return {
        "schemaVersion": 1,
        "target": "react-native-android",
        "application": application,
        **application_identity,
        "identity": identity,
        "affectedApkSha256": f"sha256:{validate_apk(affected_apk)}",
        "fixedApkSha256": f"sha256:{validate_apk(fixed_apk)}",
        "affected": affected,
        "fixed": fixed,
        "neighboring": neighbors,
        "corpus": corpus,
        "runtime": runtime_evidence(device, runtime_preflight),
    }

def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--application", choices=("joplin", "music"), required=True)
    parser.add_argument("--sdk", required=True, type=Path)
    parser.add_argument("--avd-home", required=True, type=Path)
    parser.add_argument("--affected-apk", required=True, type=Path)
    parser.add_argument("--fixed-apk", required=True, type=Path)
    parser.add_argument("--fixture-dir", type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--cli-commit", required=True)
    parser.add_argument("--runs", type=int, choices=range(1, 4), default=3)
    parser.add_argument("--with-corpus", action="store_true")
    args = parser.parse_args()
    if re.fullmatch(r"[0-9a-f]{40}", args.cli_commit) is None:
        parser.error("--cli-commit must be a full lowercase Git commit")
    if args.application == "music" and args.fixture_dir is None:
        parser.error("--fixture-dir is required for Music")
    args.evidence.mkdir(parents=True, exist_ok=True)
    device = Device(args.sdk, args.avd_home, args.evidence)
    result = None
    try:
        result = campaign(
            args.application,
            device,
            args.affected_apk,
            args.fixed_apk,
            args.fixture_dir,
            args.runs,
            args.with_corpus,
        )
    finally:
        device.stop()
        audit = cleanup_audit(device)
        (args.evidence / "cleanup-audit.json").write_text(
            json.dumps(audit, indent=2) + "\n",
            encoding="utf-8",
        )
        if not audit["passed"]:
            raise RuntimeError(f"campaign cleanup audit failed: {audit!r}")
    assert result is not None
    result["cliCommit"] = args.cli_commit
    if args.fixture_dir is not None:
        result["fixtureSha256"] = music_fixture_hashes(args.fixture_dir)
    output = args.evidence / f"{args.application}-campaign.json"
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
