#!/usr/bin/env python3
"""Run the bounded Gitea and Memos backend-contract field campaign."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import math
import os
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any

CURL_IMAGE = (
    "curlimages/curl@"
    "sha256:b3f1fb2a51d923260350d21b8654bbc607164a987e2f7c84a0ac199a67df812a"
)
PASSWORD = "field-only-password-7f3c"
REVISIONS = {
    "gitea": {
        "affected": "98c61942aa433342eacf08e4040ded80b1d0efe1",
        "fixed": "4812e354866a066dcb899af667b0fad5fa094065",
    },
    "memos": {
        "affected": "14fb38f37560541bf2719647e7e8b1468937f8ef",
        "fixed": "7c3fcc297d8e5a955d9c0bc4f3ca917854132e8e",
    },
}
IMAGES = {
    "gitea": {
        "affected": "reproit-field-gitea:98c61942",
        "fixed": "reproit-field-gitea:4812e354",
    },
    "memos": {
        "affected": "reproit-field-memos:14fb38f3",
        "fixed": "reproit-field-memos:7c3fcc29",
    },
}
IDENTITIES = {
    "gitea": "filtered-commit-total-count-ignores-bounds",
    "memos": "public-memo-list-requires-authentication",
}


def execute(arguments: list[str], timeout_seconds: int = 300) -> str:
    completed = subprocess.run(
        arguments,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=timeout_seconds,
    )
    return completed.stdout.strip()


def best_effort(arguments: list[str]) -> None:
    try:
        subprocess.run(
            arguments,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=60,
        )
    except subprocess.TimeoutExpired:
        pass


def resource_absent(arguments: list[str]) -> bool:
    try:
        result = subprocess.run(
            arguments,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=30,
        )
    except subprocess.TimeoutExpired:
        return False
    return result.returncode != 0


def capture(arguments: list[str], timeout_seconds: int = 300) -> str:
    completed = subprocess.run(
        arguments,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=timeout_seconds,
    )
    return completed.stdout.strip()


def curl_request(
    network: str,
    url: str,
    *,
    method: str = "GET",
    headers: list[str] | None = None,
    body: dict[str, Any] | None = None,
    basic_auth: bool = False,
) -> tuple[int, dict[str, str], str]:
    arguments = [
        "docker",
        "run",
        "--rm",
        "--network",
        network,
        CURL_IMAGE,
        "-sS",
        "--connect-timeout",
        "1",
        "--max-time",
        "5",
        "-X",
        method,
        "-D",
        "-",
        "-o",
        "-",
        "-w",
        "\n__REPROIT_STATUS__:%{http_code}",
    ]
    if basic_auth:
        arguments.extend(["--user", f"reproit:{PASSWORD}"])
    for header in headers or []:
        arguments.extend(["-H", header])
    if body is not None:
        arguments.extend(
            ["-H", "Content-Type: application/json", "--data-binary", json.dumps(body)]
        )
    arguments.append(url)
    output = execute(arguments, timeout_seconds=10)
    payload, status_text = output.rsplit("\n__REPROIT_STATUS__:", 1)
    normalized = payload.replace("\r\n", "\n")
    header_text, response_body = normalized.split("\n\n", 1)
    parsed_headers = {}
    for line in header_text.splitlines()[1:]:
        name, separator, value = line.partition(":")
        if separator:
            parsed_headers[name.lower()] = value.strip()
    return int(status_text), parsed_headers, response_body


def wait_ready(network: str, service: str, url: str) -> int:
    for attempt in range(1, 61):
        try:
            status, _, _ = curl_request(network, url)
            if 200 <= status < 500:
                return attempt
        except (subprocess.SubprocessError, ValueError):
            pass
        running = execute(
            ["docker", "inspect", service, "--format", "{{.State.Running}}"]
        )
        if running != "true":
            log = capture(["docker", "logs", service])
            raise RuntimeError(f"{service} exited before readiness:\n{log}")
        time.sleep(0.5)
    raise RuntimeError(f"service did not become ready: {url}")


def image_metadata(image: str, expected_revision: str) -> dict[str, Any]:
    output = execute(["docker", "image", "inspect", image])
    inspection = json.loads(output)[0]
    labels = inspection["Config"].get("Labels") or {}
    if labels.get("org.opencontainers.image.revision") != expected_revision:
        raise RuntimeError(f"{image} does not carry revision {expected_revision}")
    return {
        "reference": image,
        "id": inspection["Id"],
        "repoDigests": inspection.get("RepoDigests") or [],
        "architecture": inspection["Architecture"],
        "revision": expected_revision,
    }


def seed_gitea(network: str, service: str) -> tuple[dict[str, Any], dict[str, Any]]:
    execute(
        [
            "docker",
            "exec",
            "--user",
            "git",
            service,
            "gitea",
            "admin",
            "user",
            "create",
            "--admin",
            "--username",
            "reproit",
            "--password",
            PASSWORD,
            "--email",
            "field@example.invalid",
            "--must-change-password=false",
        ]
    )
    root = f"http://{service}:3000/api/v1"
    status, _, _ = curl_request(
        network,
        f"{root}/user/repos",
        method="POST",
        body={"name": "field", "private": True, "default_branch": "main"},
        basic_auth=True,
    )
    if status != 201:
        raise RuntimeError(f"Gitea repository creation returned {status}")
    file_sha = None
    for index in range(1, 5):
        timestamp = f"2024-01-0{index}T00:00:00Z"
        request = {
            "content": base64.b64encode(f"revision {index}\n".encode()).decode(),
            "message": f"seed {index}",
            "branch": "main",
            "dates": {"author": timestamp, "committer": timestamp},
        }
        if file_sha is not None:
            request["sha"] = file_sha
        status, _, response = curl_request(
            network,
            f"{root}/repos/reproit/field/contents/record.txt",
            method="PUT",
            body=request,
            basic_auth=True,
        )
        if status not in {200, 201}:
            raise RuntimeError(f"Gitea seed commit {index} returned {status}")
        file_sha = json.loads(response)["content"]["sha"]
    return {"root": root, "commits": 4}, {"username": "reproit"}


def probe_gitea(network: str, service: str, seed: dict[str, Any]) -> dict[str, Any]:
    root = seed["root"]
    filtered_url = (
        f"{root}/repos/reproit/field/commits?sha=main"
        "&since=2024-01-02T12%3A00%3A00Z"
        "&until=2024-01-03T12%3A00%3A00Z&limit=50"
    )
    start = time.monotonic()
    status, headers, body = curl_request(network, filtered_url, basic_auth=True)
    elapsed = time.monotonic() - start
    filtered = json.loads(body)
    total = int(headers["x-total-count"])
    control_status, control_headers, control_body = curl_request(
        network,
        f"{root}/repos/reproit/field/commits?sha=main&limit=50",
        basic_auth=True,
    )
    control = json.loads(control_body)
    if status != 200 or len(filtered) != 1:
        raise RuntimeError("Gitea filtered observation was not reached")
    if control_status != 200 or int(control_headers["x-total-count"]) != len(control):
        raise RuntimeError("Gitea neighboring unfiltered control failed")
    swagger_status, _, swagger = curl_request(
        network, f"http://{service}:3000/swagger.v1.json"
    )
    if swagger_status != 200:
        raise RuntimeError("Gitea did not serve its authored Swagger contract")
    return {
        "elapsedSeconds": round(elapsed, 6),
        "filteredStatus": status,
        "filteredBodyCount": len(filtered),
        "filteredHeaderCount": total,
        "unfilteredBodyCount": len(control),
        "unfilteredHeaderCount": int(control_headers["x-total-count"]),
        "contractSha256": hashlib.sha256(swagger.encode()).hexdigest(),
        "identity": IDENTITIES["gitea"] if total != len(filtered) else None,
        "neighboringLegalBehavior": "unfiltered X-Total-Count equals the response body count",
    }


def seed_memos(network: str, service: str) -> tuple[dict[str, Any], dict[str, Any]]:
    root = f"http://{service}:5230"
    status, _, _ = curl_request(
        network,
        f"{root}/memos.api.v1.UserService/CreateUser",
        method="POST",
        headers=["Connect-Protocol-Version: 1"],
        body={
            "user": {
                "username": "host",
                "password": PASSWORD,
                "displayName": "Host",
            }
        },
    )
    if status != 200:
        raise RuntimeError(f"Memos host creation returned {status}")
    status, _, response = curl_request(
        network,
        f"{root}/memos.api.v1.AuthService/SignIn",
        method="POST",
        headers=["Connect-Protocol-Version: 1"],
        body={
            "passwordCredentials": {
                "username": "host",
                "password": PASSWORD,
            }
        },
    )
    if status != 200:
        raise RuntimeError(f"Memos sign-in returned {status}")
    token = json.loads(response)["accessToken"]
    auth = [f"Authorization: Bearer {token}"]
    for visibility in ("PUBLIC", "PRIVATE"):
        status, _, _ = curl_request(
            network,
            f"{root}/api/v1/memos",
            method="POST",
            headers=auth,
            body={
                "state": "NORMAL",
                "content": f"{visibility.lower()} field marker",
                "visibility": visibility,
            },
        )
        if status != 200:
            raise RuntimeError(f"Memos {visibility} seed returned {status}")
    return {"root": root, "authorization": auth[0]}, {"seeded": ["PUBLIC", "PRIVATE"]}


def memo_count(response_body: str, content: str) -> int:
    document = json.loads(response_body)
    return sum(memo.get("content") == content for memo in document.get("memos", []))


def probe_memos(network: str, service: str, seed: dict[str, Any]) -> dict[str, Any]:
    root = seed["root"]
    start = time.monotonic()
    status, _, body = curl_request(network, f"{root}/api/v1/memos")
    elapsed = time.monotonic() - start
    auth_status, _, auth_body = curl_request(
        network,
        f"{root}/api/v1/memos",
        headers=[seed["authorization"]],
    )
    protected_status, _, _ = curl_request(network, f"{root}/api/v1/auth/me")
    if auth_status != 200 or memo_count(auth_body, "public field marker") != 1:
        raise RuntimeError("Memos authenticated observation control failed")
    if protected_status != 401:
        raise RuntimeError("Memos protected-route neighboring control failed")
    public_count = memo_count(body, "public field marker") if status == 200 else 0
    private_count = memo_count(body, "private field marker") if status == 200 else 0
    if status == 200 and (public_count != 1 or private_count != 0):
        raise RuntimeError("Memos anonymous visibility filtering failed")
    return {
        "elapsedSeconds": round(elapsed, 6),
        "anonymousStatus": status,
        "anonymousPublicCount": public_count,
        "anonymousPrivateCount": private_count,
        "authenticatedStatus": auth_status,
        "protectedStatus": protected_status,
        "identity": IDENTITIES["memos"] if status == 401 else None,
        "neighboringLegalBehavior": "a genuinely protected identity route returns 401",
    }


def container_arguments(
    application: str,
    name: str,
    network: str,
    image: str,
    data_root: Path,
    session: str,
) -> list[str]:
    port = "3000" if application == "gitea" else "5230"
    arguments = [
        "docker",
        "run",
        "-d",
        "--name",
        name,
        "--network",
        network,
        "--label",
        f"reproit.field.session={session}",
        "--read-only",
        "--tmpfs",
        "/tmp:rw,noexec,nosuid,size=64m",
        "--publish",
        f"127.0.0.1::{port}",
        "--mount",
        f"type=bind,src={data_root},dst={'/data' if application == 'gitea' else '/var/opt/memos'}",
    ]
    if application == "gitea":
        arguments.extend(
            [
                "--entrypoint",
                "/app/gitea/gitea",
                "--user",
                "1000:1000",
                "--tmpfs",
                "/run:rw,noexec,nosuid,size=16m",
                "-e",
                "GITEA__database__DB_TYPE=sqlite3",
                "-e",
                "GITEA__database__PATH=/data/gitea/gitea.db",
                "-e",
                "GITEA__repository__DEFAULT_BRANCH=main",
                "-e",
                "GITEA__security__INSTALL_LOCK=true",
                "-e",
                "GITEA__server__DISABLE_SSH=true",
                "-e",
                "GITEA__service__DISABLE_REGISTRATION=true",
                "-e",
                "GITEA_WORK_DIR=/data/gitea",
            ]
        )
    arguments.append(image)
    if application == "gitea":
        arguments.append("web")
    return arguments


def prepare_gitea_data(data_root: Path) -> None:
    config_root = data_root / "gitea" / "conf"
    config_root.mkdir(parents=True)
    config = """RUN_MODE = prod

[database]
DB_TYPE = sqlite3
PATH = /data/gitea/gitea.db
SQLITE_TIMEOUT = 500

[repository]
ROOT = /data/git/repositories
DEFAULT_BRANCH = main

[server]
HTTP_ADDR = 0.0.0.0
HTTP_PORT = 3000
DISABLE_SSH = true

[security]
INSTALL_LOCK = true
SECRET_KEY = disposable-field-secret
INTERNAL_TOKEN = disposable-field-internal-token

[service]
DISABLE_REGISTRATION = true

[log]
MODE = console
LEVEL = Info
"""
    (config_root / "app.ini").write_text(config, encoding="utf-8")


def run_trial(
    application: str,
    revision_kind: str,
    run_number: int,
    output: Path,
    session: str,
) -> dict[str, Any]:
    revision = REVISIONS[application][revision_kind]
    image = IMAGES[application][revision_kind]
    metadata = image_metadata(image, revision)
    name = f"reproit-{application}-{revision_kind}-{run_number}-{os.getpid()}"
    network = f"{name}-net"
    runtime_root = output / "runtime" / name
    data_root = runtime_root / "data"
    evidence_root = output / "runs" / application / f"{revision_kind}-{run_number}"
    shutil.rmtree(runtime_root, ignore_errors=True)
    shutil.rmtree(evidence_root, ignore_errors=True)
    data_root.mkdir(parents=True)
    if application == "gitea":
        prepare_gitea_data(data_root)
    evidence_root.mkdir(parents=True)
    started = time.monotonic()
    cleanup = {"container": False, "network": False, "runtime": False}
    try:
        execute(["docker", "network", "create", "--internal", network])
        execute(container_arguments(application, name, network, image, data_root, session))
        port = "3000" if application == "gitea" else "5230"
        readiness_attempt = wait_ready(network, name, f"http://{name}:{port}/")
        if application == "gitea":
            seed, seed_record = seed_gitea(network, name)
        else:
            seed, seed_record = seed_memos(network, name)
        setup_seconds = math.ceil(time.monotonic() - started)
        observation = (
            probe_gitea(network, name, seed)
            if application == "gitea"
            else probe_memos(network, name, seed)
        )
        inspection = json.loads(execute(["docker", "inspect", name]))[0]
        configured_binding = inspection["HostConfig"]["PortBindings"][f"{port}/tcp"][0]
        actual_bindings = inspection["NetworkSettings"]["Ports"].get(f"{port}/tcp") or []
        binding = actual_bindings[0] if actual_bindings else configured_binding
        if configured_binding["HostIp"] != "127.0.0.1":
            raise RuntimeError(f"{name} was not bound to loopback")
        network_info = json.loads(execute(["docker", "network", "inspect", network]))[0]
        if network_info["Internal"] is not True:
            raise RuntimeError(f"{network} permits external egress")
        service_log = capture(["docker", "logs", name])
        (evidence_root / "service.log").write_text(service_log, encoding="utf-8")
        record = {
            "application": application,
            "revisionKind": revision_kind,
            "run": run_number,
            "cleanLaunch": True,
            "observationReached": True,
            "exceptions": [],
            "jsHeapMiB": None,
            "setupSeconds": setup_seconds,
            "readinessAttempts": readiness_attempt,
            "image": metadata,
            "runtime": {
                "networkInternal": True,
                "publishedHost": configured_binding["HostIp"],
                "publishedPort": binding["HostPort"] or None,
                "readOnlyRoot": inspection["HostConfig"]["ReadonlyRootfs"],
                "dataDirectoryEmptyBeforeStart": True,
            },
            "seed": seed_record,
            "observation": observation,
        }
    finally:
        best_effort(["docker", "rm", "-f", name])
        cleanup["container"] = resource_absent(["docker", "inspect", name])
        best_effort(["docker", "network", "rm", network])
        cleanup["network"] = resource_absent(
            ["docker", "network", "inspect", network]
        )
        shutil.rmtree(runtime_root, ignore_errors=True)
        cleanup["runtime"] = not runtime_root.exists()
    if not all(cleanup.values()):
        raise RuntimeError(f"incomplete cleanup for {name}: {cleanup}")
    record["cleanup"] = cleanup
    encoded = json.dumps(record, indent=2, sort_keys=True) + "\n"
    (evidence_root / "record.json").write_text(encoded, encoding="utf-8")
    record["rawRecordSha256"] = hashlib.sha256(encoded.encode()).hexdigest()
    return record


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("target/reproit-validation/backend-contract-field"),
    )
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument(
        "--application",
        choices=("all", "gitea", "memos"),
        default="all",
    )
    parser.add_argument(
        "--revision-kind",
        choices=("all", "affected", "fixed"),
        default="all",
    )
    args = parser.parse_args()
    if not 1 <= args.runs <= 3:
        raise ValueError("--runs must be between 1 and 3")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    session = f"backend-contract-{os.getpid()}"
    records = []
    applications = (
        ("gitea", "memos")
        if args.application == "all"
        else (args.application,)
    )
    revision_kinds = (
        ("affected", "fixed")
        if args.revision_kind == "all"
        else (args.revision_kind,)
    )
    for application in applications:
        for revision_kind in revision_kinds:
            for run_number in range(1, args.runs + 1):
                records.append(
                    run_trial(
                        application,
                        revision_kind,
                        run_number,
                        output,
                        session,
                    )
                )
    remaining = execute(
        [
            "docker",
            "ps",
            "-aq",
            "--filter",
            f"label=reproit.field.session={session}",
        ]
    )
    if remaining:
        raise RuntimeError(f"campaign containers remain: {remaining}")
    summary = {
        "schemaVersion": 1,
        "session": session,
        "curlImage": CURL_IMAGE,
        "runsPerRevision": args.runs,
        "containersRemaining": 0,
        "records": records,
    }
    summary_path = output / "summary.json"
    summary_path.write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(summary_path)


if __name__ == "__main__":
    main()
