#!/usr/bin/env python3
"""Validate Bokkie's backend/UI Rust toolchain boundary."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BUILD_RUST = "1.99.0"
RUST_IMAGE = (
    "rust:1.99.0-bookworm@sha256:"
    "b36c246742b4d323472588f789601be34eb4af053255e2af53969078ee8dcec7"
)
SELECTOR_FILES = (
    "tools/check-ui.sh",
    "tools/qualify-ui.sh",
    ".github/workflows/ci.yml",
    "deploy/Dockerfile",
    "tools/container-probe/Dockerfile",
)


def load(path: Path) -> dict:
    with path.open("rb") as source:
        return tomllib.load(source)


def validate(root: Path) -> list[str]:
    expected = {
        "rust-toolchain.toml": ("toolchain.channel", BUILD_RUST),
        "apps/bokkie-attention-ui/rust-toolchain.toml": ("toolchain.channel", BUILD_RUST),
        "Cargo.toml": ("package.rust-version", "1.85"),
        "crates/bokkie-operator-api/Cargo.toml": ("package.rust-version", "1.85"),
        "apps/bokkie-attention-ui/Cargo.toml": ("package.rust-version", "1.97"),
    }
    problems: list[str] = []
    documents: dict[str, dict] = {}
    for relative in expected:
        try:
            documents[relative] = load(root / relative)
        except (FileNotFoundError, tomllib.TOMLDecodeError) as error:
            problems.append(f"{relative}: cannot read TOML: {error}")

    for relative, (key, wanted) in expected.items():
        if relative not in documents:
            continue
        try:
            value: object = documents[relative]
            for component in key.split("."):
                value = value[component]  # type: ignore[index]
        except KeyError as error:
            problems.append(f"{relative}: cannot read {key}: {error}")
            continue
        if value != wanted:
            problems.append(f"{relative}: {key} must be {wanted!r}, observed {value!r}")

    backend = documents.get("rust-toolchain.toml", {}).get("toolchain")
    if backend is not None and set(backend.get("components", [])) != {"clippy", "rustfmt"}:
        problems.append("rust-toolchain.toml: backend pin must include clippy and rustfmt")
    ui = documents.get("apps/bokkie-attention-ui/rust-toolchain.toml", {}).get("toolchain")
    if ui is not None:
        if set(ui.get("components", [])) != {"clippy", "rustfmt"}:
            problems.append("UI toolchain pin must include clippy and rustfmt")
        if "wasm32-unknown-unknown" not in ui.get("targets", []):
            problems.append("UI toolchain pin must include wasm32-unknown-unknown")

    for relative in SELECTOR_FILES:
        try:
            text = (root / relative).read_text(encoding="utf-8")
        except OSError as error:
            problems.append(f"{relative}: cannot read build selectors: {error}")
            continue
        if relative.endswith(".sh") or relative.endswith("Dockerfile"):
            selectors = re.findall(r"\bcargo\s+(\S+)", text)
            if not selectors or any(value != f"+{BUILD_RUST}" for value in selectors):
                problems.append(f"{relative}: every Cargo command must select +{BUILD_RUST}")
        if relative.endswith(".yml"):
            selectors = re.findall(r"^\s+RUSTUP_TOOLCHAIN:\s*(\S+)\s*$", text, re.MULTILINE)
            if selectors != [BUILD_RUST, BUILD_RUST]:
                problems.append(f"{relative}: backend and UI CI must select {BUILD_RUST}")
        if relative.endswith("Dockerfile"):
            images = re.findall(r"^FROM\s+(rust:\S+)", text, re.MULTILINE)
            count = 2 if relative == "deploy/Dockerfile" else 1
            if images != [RUST_IMAGE] * count:
                problems.append(f"{relative}: Rust builders must use the qualified digest-pinned image")

    # The CLI must match the locked library that generates the browser module.
    try:
        packages = load(root / "Cargo.lock")["package"]
        versions = [package["version"] for package in packages if package["name"] == "wasm-bindgen"]
        dockerfile = (root / "deploy/Dockerfile").read_text(encoding="utf-8")
        cli_versions = re.findall(r"install\s+--locked\s+--version\s+(\S+)\s+wasm-bindgen-cli", dockerfile)
        if versions != cli_versions:
            problems.append("deploy/Dockerfile: wasm-bindgen CLI must match the locked library")
    except (OSError, KeyError, tomllib.TOMLDecodeError) as error:
        problems.append(f"cannot validate wasm-bindgen library/CLI boundary: {error}")
    return problems


def main() -> int:
    problems = validate(ROOT)
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        return 1
    print(f"toolchain contract passed: build {BUILD_RUST}; backend MSRV 1.85; UI MSRV 1.97")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
