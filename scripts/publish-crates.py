#!/usr/bin/env python3
"""Publish the two crates in order, safely resuming the same release revision."""

import argparse
import json
import subprocess
import time
import urllib.error
import urllib.request

CRATES = ("ferralk-glob", "ferralk")


def registry_version(name, version):
    """Only a successful sparse-index response or HTTP 404 establishes absence."""
    url = f"https://index.crates.io/{name[:2]}/{name[2:4]}/{name}"
    request = urllib.request.Request(url, headers={"User-Agent": "ferralk-release"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            records = response.read().decode("utf-8").splitlines()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise
    # Parse every record: a malformed registry response must never mean absent.
    entries = [json.loads(line) for line in records]
    if not entries or any(not isinstance(entry.get("vers"), str) for entry in entries):
        raise ValueError(f"Invalid registry index for {name}")
    return next((entry for entry in entries if entry["vers"] == version), None)


def assert_available(name, version, entry):
    if entry.get("yanked") is not False:
        raise RuntimeError(f"{name}@{version} is yanked or has invalid registry metadata")


def publish(packages, *, lookup=registry_version, run=subprocess.run, sleep=time.sleep):
    for name, version in packages:
        entry = lookup(name, version)
        if entry is not None:
            assert_available(name, version, entry)
            print(f"{name}@{version} already published; skipping upload", flush=True)
            continue
        # Authorization and validation errors stay failures. Do not retry them
        # as though every Cargo error were just index propagation.
        run(["cargo", "publish", "--locked", "-p", name], check=True)
        for attempt in range(10):
            entry = lookup(name, version)
            if entry is not None:
                assert_available(name, version, entry)
                break
            if attempt == 9:
                raise RuntimeError(f"{name}@{version} did not become visible in the registry")
            sleep(15)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="package both crates without uploading")
    args = parser.parse_args()
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True
    ))
    versions = {package["name"]: package["version"] for package in metadata["packages"]}
    packages = [(name, versions[name]) for name in CRATES]
    if args.dry_run:
        for name, _ in packages:
            subprocess.run(["cargo", "package", "--locked", "--no-verify", "-p", name], check=True)
    else:
        publish(packages)


if __name__ == "__main__":
    main()
