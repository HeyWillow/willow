#!/usr/bin/env python3
"""Verify that Willow's application boundary remains Rust-only."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

EXPECTED_MAIN = b"void __attribute__((weak)) app_main(void) {}\n"
MAX_APP_SIZE = 0x300000
FORBIDDEN_BUILD_TOKENS = (
    b"esp-adf",
    b"esp_adf",
    b"audio_pipeline",
    b"audio_element",
    b"esp_periph",
)
BUILD_GRAPH_FILES = (
    "build.ninja",
    "CMakeCache.txt",
    "compile_commands.json",
    "project_description.json",
)


def parse_arguments() -> argparse.Namespace:
    """Parse paths produced by the Cargo-first build."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", required=True, type=Path)
    parser.add_argument("--elf", required=True, type=Path)
    parser.add_argument("--idf-build", required=True, type=Path)
    parser.add_argument("--staged-build", required=True, type=Path)
    return parser.parse_args()


def check_main(project: Path, failures: list[str]) -> None:
    """Require the exact one-file native bridge."""
    main = project / "main"
    entries = sorted(path.name for path in main.iterdir())
    if entries != ["main.c"]:
        failures.append(f"main/ must contain only main.c; found {entries}")
        return

    source = main / "main.c"
    if source.is_symlink() or not source.is_file():
        failures.append("main/main.c must be a regular file")
    elif source.read_bytes() != EXPECTED_MAIN:
        failures.append("main/main.c does not match the required weak stub")


def check_rust_declarations(project: Path, failures: list[str]) -> None:
    """Reject source-level ABI hooks for local Willow C code."""
    extern_block = re.compile(r'\b(?:unsafe\s+)?extern\s*"C"\s*\{')
    no_mangle = re.compile(r"#\s*\[\s*(?:unsafe\s*\(\s*)?no_mangle")

    for source in sorted((project / "src").rglob("*.rs")):
        text = source.read_text(encoding="utf-8")
        relative = source.relative_to(project)
        if extern_block.search(text):
            failures.append(
                f"{relative} declares a C symbol; use generated external bindings"
            )
        if no_mangle.search(text):
            failures.append(f"{relative} exports a C ABI symbol")


def version_major(version: str) -> int | None:
    """Return a version's numeric major component when it is explicit."""
    match = re.fullmatch(r"v?(\d+)(?:\.\d+){1,2}", version)
    return int(match.group(1)) if match else None


def check_esp_sr(project: Path, failures: list[str]) -> None:
    """Require both requested and resolved ESP-SR versions below 2.0."""
    manifest = project / "idf" / "willow-build" / "idf_component.yml"
    requested = re.findall(
        r"^\s*espressif/esp-sr:\s*['\"]?([^'\"\s#]+)",
        manifest.read_text(encoding="utf-8"),
        flags=re.MULTILINE,
    )
    if len(requested) != 1 or version_major(requested[0]) not in range(0, 2):
        failures.append(f"ESP-SR manifest version must be below 2.0; found {requested}")

    lock = (project / "components_esp32s3.lock").read_text(encoding="utf-8")
    resolved = re.findall(
        r"^  espressif/esp-sr:\n(?:(?:    .*|\s*)\n)*?"
        r"^    version:\s*['\"]?([^'\"\s#]+)",
        lock,
        flags=re.MULTILINE,
    )
    if len(resolved) != 1 or version_major(resolved[0]) not in range(0, 2):
        failures.append(f"resolved ESP-SR version must be below 2.0; found {resolved}")


def contains_forbidden_token(data: bytes) -> bytes | None:
    """Return the first forbidden ESP-ADF marker in lower-cased data."""
    lowered = data.lower()
    return next((token for token in FORBIDDEN_BUILD_TOKENS if token in lowered), None)


def check_build_paths(root: Path, label: str, failures: list[str]) -> None:
    """Reject ESP-ADF paths cloned, installed, or staged in a build tree."""
    for path in root.rglob("*"):
        token = contains_forbidden_token(path.as_posix().encode())
        if token is not None:
            failures.append(
                f"{label} contains forbidden path {path} ({token.decode()})"
            )
            return


def check_build_graph(idf_build: Path, failures: list[str]) -> None:
    """Reject ESP-ADF references from generated component and link metadata."""
    for filename in BUILD_GRAPH_FILES:
        path = idf_build / filename
        if not path.is_file():
            failures.append(f"missing ESP-IDF build metadata {path}")
            continue
        token = contains_forbidden_token(path.read_bytes())
        if token is not None:
            failures.append(f"{path} references forbidden {token.decode()} code")


def check_outputs(arguments: argparse.Namespace, failures: list[str]) -> None:
    """Check the linked ELF, staged trees, and OTA application size."""
    for path, label in ((arguments.elf, "firmware ELF"), (arguments.app, "app image")):
        if not path.is_file():
            failures.append(f"missing {label}: {path}")

    if arguments.elf.is_file():
        token = contains_forbidden_token(arguments.elf.read_bytes())
        if token is not None:
            failures.append(f"firmware ELF contains forbidden {token.decode()} code")

    if arguments.app.is_file() and arguments.app.stat().st_size > MAX_APP_SIZE:
        app_size = arguments.app.stat().st_size
        failures.append(
            f"app image is {app_size} bytes; limit is {MAX_APP_SIZE}"
        )

    check_build_paths(arguments.idf_build.parent, "ESP-IDF project", failures)
    check_build_paths(arguments.staged_build, "staged build", failures)
    check_build_graph(arguments.idf_build, failures)


def main() -> int:
    """Run every Rust-only boundary check."""
    arguments = parse_arguments()
    project = Path(__file__).resolve().parent.parent
    failures: list[str] = []

    check_main(project, failures)
    check_rust_declarations(project, failures)
    check_esp_sr(project, failures)
    check_outputs(arguments, failures)

    if failures:
        for failure in failures:
            print(f"Rust-only boundary check failed: {failure}", file=sys.stderr)
        return 1

    print("Rust-only Willow boundary verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
