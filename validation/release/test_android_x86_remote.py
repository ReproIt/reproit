#!/usr/bin/env python3

from __future__ import annotations

import unittest
from pathlib import Path

RELEASE = Path(__file__).resolve().parent
COLLECTOR = RELEASE / "run-android-x86-remote.sh"
WORKER = RELEASE / "android-x86" / "remote-worker.sh"
ISOLATED = RELEASE / "android-x86" / "run-isolated.sh"
DOCKERFILE = RELEASE / "android-x86" / "Dockerfile"


class AndroidX86RemoteContractTests(unittest.TestCase):
    def test_exact_mode_rejects_dirty_source(self) -> None:
        source = COLLECTOR.read_text(encoding="utf-8")
        self.assertIn("exact mode requires a clean worktree", source)
        self.assertIn("git -C \"$ROOT\" status --porcelain=v1", source)

    def test_runtime_is_network_isolated_and_kvm_backed(self) -> None:
        source = WORKER.read_text(encoding="utf-8")
        self.assertIn("--network none", source)
        self.assertIn("--device /dev/kvm", source)
        self.assertIn("--env RUSTUP_TOOLCHAIN=stable", source)
        self.assertIn("rm -rf /owned/source /owned/avd", source)
        self.assertIn('rm -rf "$SOURCE" "$AVD"', source)

    def test_pins_emulator_archive_and_system_image(self) -> None:
        source = ISOLATED.read_text(encoding="utf-8")
        self.assertIn("36.2.12", source)
        self.assertIn("14214601", source)
        self.assertIn(
            "55e7ce272dd27413855c81b4629c107a1553d07a2f77fa55a6049fdca22f4221",
            source,
        )
        self.assertIn(
            "eb4bd8cc387915563a0a051c51ac58012e183e1bd21bb0fe2e82f1b255de45a1",
            source,
        )
        self.assertIn(
            "4566663c3876e022b4fa4ced8c8697c4ab1688267f090114fd92d027b32e619b",
            source,
        )
        self.assertIn(
            "6cea1df3efb77103ac3e2beb9bf4718964b0e0869ab16d39d29d5cbae1c147ad",
            source,
        )
        self.assertIn(
            "d1096e11aba9c974644369ee3c50d239acac3f3428ffa928e5b9c14dfb7a57de",
            source,
        )
        self.assertIn("28.2.13676358", source)
        self.assertIn(
            "4dab4ba20f79dc510ce760110d897d07a89d7389c2af162c416d14133a7102c7",
            source,
        )
        self.assertIn(
            "6156dd4e5e466333197a00b8d20bca72c292186643b749dcaaa3aa9164afb1de",
            source,
        )
        self.assertIn("3.22.1", source)
        self.assertIn(
            "c8c39aee0443330e9b1866e1d85cc2405a4eec5dfbb468c5017c3eaecb4964f5",
            source,
        )
        self.assertIn(
            "6fa84be1efc3ab25d1cf397d0bb35891e5f99316a35d89cd8c04be5898730174",
            source,
        )

    def test_runtime_requires_real_x86_64_api_36_boot(self) -> None:
        source = ISOLATED.read_text(encoding="utf-8")
        self.assertIn('getprop ro.product.cpu.abi', source)
        self.assertIn('= "x86_64"', source)
        self.assertIn('getprop ro.build.version.sdk', source)
        self.assertIn('= "36"', source)
        self.assertIn("getprop sys.boot_completed", source)

    def test_container_sources_and_framework_tools_are_pinned(self) -> None:
        source = DOCKERFILE.read_text(encoding="utf-8")
        self.assertEqual(source.count("FROM "), 3)
        self.assertEqual(source.count("@sha256:"), 2)
        self.assertIn("FROM rust:bookworm AS rust", source)
        self.assertIn("rustup update stable", source)
        self.assertIn("flutter_linux_3.41.6-stable.tar.xz", source)
        self.assertIn("appium@3.5.2", source)
        self.assertIn("uiautomator2@8.0.0", source)


if __name__ == "__main__":
    unittest.main()
