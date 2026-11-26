#!/usr/bin/env python3
"""Publish the two crates in order, safely resuming the same release revision."""

import argparse
from datetime import datetime, timezone
import email.utils
import http.client
import json
import math
import subprocess
import time
import urllib.error
import urllib.request

CRATES = ("ferralk-glob", "ferralk")
POLL_DELAY_SECONDS = 15
MAX_RETRY_AFTER_SECONDS = 60


def registry_version(name, version):
    """Only a successful sparse-index response or HTTP 404 establishes absence."""
    url = f"https://index.crates.io/{name[:2]}/{name[2:4]}/{name}"
    request = urllib.request.Request(
        url,
        headers={
            "User-Agent": "ferralk-release",
            "Cache-Control": "no-cache",
        },
    )
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


def is_transient_lookup_error(error):
    if isinstance(error, urllib.error.HTTPError):
        return error.code == 429 or 500 <= error.code < 600
    return isinstance(
        error, (urllib.error.URLError, TimeoutError, http.client.HTTPException, OSError)
    )


def parse_retry_after(raw, *, now=None):
    text = str(raw).strip()
    if not text:
        return None
    try:
        return max(0, int(text))
    except ValueError:
        pass
    try:
        retry_at = email.utils.parsedate_to_datetime(text)
    except (TypeError, ValueError):
        return None
    if retry_at is None:
        return None
    if retry_at.tzinfo is None:
        retry_at = retry_at.replace(tzinfo=timezone.utc)
    current = datetime.now(timezone.utc) if now is None else now()
    if current.tzinfo is None:
        current = current.replace(tzinfo=timezone.utc)
    delta = (retry_at - current).total_seconds()
    return max(0, math.ceil(delta))


def retry_delay(error, base_delay=POLL_DELAY_SECONDS, *, now=None):
    if isinstance(error, urllib.error.HTTPError) and error.headers is not None:
        raw = error.headers.get("Retry-After")
        if raw is None:
            raw = error.headers.get("retry-after")
        if raw is not None:
            seconds = parse_retry_after(raw, now=now)
            if seconds is not None:
                if seconds > MAX_RETRY_AFTER_SECONDS:
                    raise RuntimeError(
                        f"Registry requested Retry-After wait of {seconds}s, "
                        f"exceeding the {MAX_RETRY_AFTER_SECONDS}s retry budget"
                    ) from error
                return max(base_delay, seconds)
    return base_delay


def assert_available(name, version, entry):
    if entry.get("yanked") is not False:
        raise RuntimeError(f"{name}@{version} is yanked or has invalid registry metadata")


def dry_run(packages, *, run=subprocess.run):
    # Package and verify all crates in one Cargo invocation so an unpublished
    # coordinated workspace version bump resolves ferralk -> ferralk-glob locally.
    command = ["cargo", "package", "--locked"]
    for name, _ in packages:
        command.extend(["-p", name])
    run(command, check=True)


def publish(packages, *, lookup=registry_version, run=subprocess.run, sleep=time.sleep, now=None):
    for name, version in packages:
        entry = lookup(name, version)
        if entry is not None:
            assert_available(name, version, entry)
            print(f"{name}@{version} already published; skipping upload", flush=True)
            continue
        # Authorization and validation errors stay failures. Do not retry them
        # as though every Cargo error were just index propagation.
        run(["cargo", "publish", "--locked", "-p", name], check=True)
        last_error = None
        for attempt in range(10):
            try:
                entry = lookup(name, version)
            except (
                urllib.error.URLError,
                TimeoutError,
                http.client.HTTPException,
                OSError,
            ) as error:
                if not is_transient_lookup_error(error):
                    raise
                last_error = error
                entry = None
            else:
                last_error = None
            if entry is not None:
                assert_available(name, version, entry)
                break
            if attempt == 9:
                message = f"{name}@{version} did not become visible in the registry"
                if last_error is not None:
                    raise RuntimeError(f"{message}: {last_error}") from last_error
                raise RuntimeError(message)
            sleep(
                retry_delay(last_error, now=now) if last_error is not None else POLL_DELAY_SECONDS
            )


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
        dry_run(packages)
    else:
        publish(packages)


if __name__ == "__main__":
    main()
