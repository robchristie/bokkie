#!/usr/bin/env python3
"""Validate Bokkie's backend/UI Rust toolchain boundary."""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def load(path: Path) -> dict:
    with path.open("rb") as source:
        return tomllib.load(source)


def validate(root: Path) -> list[str]:
    expected = {
        "rust-toolchain.toml": ("toolchain.channel", "1.85.0"),
        "apps/bokkie-attention-ui/rust-toolchain.toml": ("toolchain.channel", "1.97.1"),
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
    return problems


def main() -> int:
    problems = validate(ROOT)
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        return 1
    print("toolchain contract passed: backend 1.85.0/MSRV 1.85; UI 1.97.1/MSRV 1.97")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
