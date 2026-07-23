#!/usr/bin/env python3
"""Repack an ESP-SR model directory in deterministic filesystem order."""

import argparse
import importlib.util
from pathlib import Path


def load_packer(path: Path):
    """Load the packer from the pinned ESP-SR component."""
    spec = importlib.util.spec_from_file_location("willow_esp_sr_pack_model", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"failed to load ESP-SR model packer from {path}")

    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main() -> None:
    """Run ESP-SR's packer with sorted directory and file traversal."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("packer", type=Path, help="ESP-SR pack_model.py path")
    parser.add_argument("model_directory", type=Path, help="staged model directory")
    args = parser.parse_args()

    packer = load_packer(args.packer)
    unsorted_walk = packer.os.walk

    def sorted_walk(*walk_args, **walk_kwargs):
        for root, directories, files in unsorted_walk(*walk_args, **walk_kwargs):
            directories.sort()
            files.sort()
            yield root, directories, files

    packer.os.walk = sorted_walk
    packer.pack_models(str(args.model_directory), "srmodels.bin")


if __name__ == "__main__":
    main()
