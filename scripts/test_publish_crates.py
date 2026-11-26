from datetime import datetime, timezone
import http.client
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import unittest
import urllib.error
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("publish_crates", Path(__file__).with_name("publish-crates.py"))
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)
PACKAGES = [("ferralk-glob", "1.2.0"), ("ferralk", "1.2.0")]
PUBLISHED = {"vers": "1.2.0", "yanked": False}


class PublishTests(unittest.TestCase):
    def test_partial_release_resumes_and_complete_release_is_a_noop(self):
        registry = {}
        uploads = []
        fail_second = True

        def cargo(command, *, check):
            self.assertTrue(check)
            name = command[-1]
            if name == "ferralk" and fail_second:
                raise subprocess.CalledProcessError(1, command)
            uploads.append(name)
            registry[name] = PUBLISHED

        def lookup(name, version):
            self.assertEqual(version, "1.2.0")
            return registry.get(name)

        with self.assertRaises(subprocess.CalledProcessError):
            publisher.publish(PACKAGES, lookup=lookup, run=cargo)
        self.assertEqual(uploads, ["ferralk-glob"])
        fail_second = False
        publisher.publish(PACKAGES, lookup=lookup, run=cargo)
        self.assertEqual(uploads, ["ferralk-glob", "ferralk"])
        publisher.publish(PACKAGES, lookup=lookup, run=cargo)
        self.assertEqual(uploads, ["ferralk-glob", "ferralk"])

    def test_waits_for_first_crate_visibility_before_second_upload(self):
        events = []
        lookups = {"ferralk-glob": 0, "ferralk": 0}

        def lookup(name, version):
            lookups[name] += 1
            events.append(("lookup", name))
            return PUBLISHED if lookups[name] >= 3 else None

        publisher.publish(PACKAGES, lookup=lookup,
                          run=lambda command, **_: events.append(("publish", command[-1])),
                          sleep=lambda _: events.append(("wait", None)))
        self.assertEqual(events[:5], [("lookup", "ferralk-glob"), ("publish", "ferralk-glob"),
                                     ("lookup", "ferralk-glob"), ("wait", None), ("lookup", "ferralk-glob")])
        self.assertEqual(events[5], ("lookup", "ferralk"))

    def test_registry_failure_and_yanked_version_never_upload(self):
        for lookup in [lambda *_: (_ for _ in ()).throw(OSError("registry unavailable")),
                       lambda *_: {"vers": "1.2.0", "yanked": True}]:
            with self.assertRaises((OSError, RuntimeError)):
                publisher.publish(PACKAGES, lookup=lookup,
                                  run=lambda *_a, **_kw: self.fail("unexpected upload"))

    def test_visibility_timeout_stops_before_second_crate(self):
        uploads = []
        with self.assertRaisesRegex(RuntimeError, "did not become visible"):
            publisher.publish(PACKAGES, lookup=lambda *_: None,
                              run=lambda command, **_: uploads.append(command[-1]), sleep=lambda _: None)
        self.assertEqual(uploads, ["ferralk-glob"])

    def test_index_distinguishes_absence_from_transport_and_invalid_data(self):
        url = "https://index.crates.io/fe/rr/ferralk"
        for code in [404, 403, 429, 500]:
            error = urllib.error.HTTPError(url, code, "failed", {}, None)
            with patch.object(publisher.urllib.request, "urlopen", side_effect=error):
                if code == 404:
                    self.assertIsNone(publisher.registry_version("ferralk", "1.2.0"))
                else:
                    with self.assertRaises(urllib.error.HTTPError):
                        publisher.registry_version("ferralk", "1.2.0")
        for payload in [b"", b"not json", b"{}"]:
            with patch.object(publisher.urllib.request, "urlopen", return_value=io.BytesIO(payload)):
                with self.assertRaises(ValueError):
                    publisher.registry_version("ferralk", "1.2.0")
        payload = json.dumps(PUBLISHED).encode()
        seen_headers = {}

        def fake_urlopen(request, timeout):
            seen_headers.update(request.header_items())
            self.assertEqual(timeout, 30)
            return io.BytesIO(payload)

        with patch.object(publisher.urllib.request, "urlopen", side_effect=fake_urlopen):
            self.assertEqual(publisher.registry_version("ferralk", "1.2.0"), PUBLISHED)
        self.assertEqual(seen_headers.get("Cache-control"), "no-cache")

    def test_dry_run_packages_all_crates_in_one_cargo_invocation(self):
        commands = []

        def cargo(command, *, check):
            self.assertTrue(check)
            commands.append(command)

        publisher.dry_run(PACKAGES, run=cargo)
        self.assertEqual(
            commands,
            [["cargo", "package", "--locked", "-p", "ferralk-glob", "-p", "ferralk"]],
        )

    def test_visibility_polling_retries_transient_errors_only(self):
        url = "https://index.crates.io/fe/rr/ferralk-glob"
        fixed_now = lambda: datetime(2026, 11, 26, 18, 0, 0, tzinfo=timezone.utc)
        transient_sequence = [
            None,  # pre-upload check
            urllib.error.HTTPError(url, 429, "too many requests", {"Retry-After": "30"}, None),
            urllib.error.HTTPError(
                url,
                429,
                "too many requests",
                {"Retry-After": "Thu, 26 Nov 2026 18:00:45 GMT"},
                None,
            ),
            urllib.error.HTTPError(
                url,
                429,
                "too many requests",
                {"Retry-After": "Thu, 26 Nov 2026 17:59:00 GMT"},
                None,
            ),
            urllib.error.HTTPError(url, 429, "too many requests", {"Retry-After": "invalid"}, None),
            urllib.error.HTTPError(url, 503, "unavailable", {}, None),
            urllib.error.URLError("connection reset"),
            TimeoutError("timed out"),
            http.client.IncompleteRead(b"partial"),
            ConnectionResetError("connection reset by peer"),
            PUBLISHED,
        ]
        calls = iter(transient_sequence)
        waits = []

        def lookup(_name, _version):
            outcome = next(calls)
            if isinstance(outcome, BaseException):
                raise outcome
            return outcome

        publisher.publish([("ferralk-glob", "1.2.0")], lookup=lookup,
                          run=lambda *_a, **_kw: None, sleep=waits.append, now=fixed_now)
        self.assertEqual(waits, [30, 45, 15, 15, 15, 15, 15, 15, 15])

        # Excessive Retry-After waits fail explicitly instead of polling early.
        for header_value in ["9999", "Thu, 26 Nov 2026 18:05:00 GMT"]:
            excessive_calls = iter([
                None,
                urllib.error.HTTPError(
                    url, 429, "too many requests", {"Retry-After": header_value}, None
                ),
            ])
            excessive_waits = []

            def excessive_lookup(_name, _version):
                outcome = next(excessive_calls)
                if isinstance(outcome, BaseException):
                    raise outcome
                return outcome

            with self.assertRaisesRegex(RuntimeError, "exceeding the 60s retry budget"):
                publisher.publish([("ferralk-glob", "1.2.0")], lookup=excessive_lookup,
                                  run=lambda *_a, **_kw: None, sleep=excessive_waits.append,
                                  now=fixed_now)
            self.assertEqual(excessive_waits, [])

        # Non-5xx HTTP errors and malformed index responses fail immediately.
        for fatal in [
            urllib.error.HTTPError(url, 403, "forbidden", {}, None),
            ValueError("Invalid registry index for ferralk-glob"),
        ]:
            fatal_calls = iter([None, fatal])
            fatal_waits = []

            def fatal_lookup(_name, _version):
                outcome = next(fatal_calls)
                if isinstance(outcome, BaseException):
                    raise outcome
                return outcome

            with self.assertRaises(type(fatal)):
                publisher.publish([("ferralk-glob", "1.2.0")], lookup=fatal_lookup,
                                  run=lambda *_a, **_kw: None, sleep=fatal_waits.append)
            self.assertEqual(fatal_waits, [])

        # Exhausting retries on transient errors surfaces a visibility timeout.
        always_502 = iter([None] + [urllib.error.HTTPError(url, 502, "bad gateway", {}, None)] * 10)

        def failing_lookup(_name, _version):
            outcome = next(always_502)
            if isinstance(outcome, BaseException):
                raise outcome
            return outcome

        with self.assertRaisesRegex(RuntimeError, "did not become visible"):
            publisher.publish([("ferralk-glob", "1.2.0")], lookup=failing_lookup,
                              run=lambda *_a, **_kw: None, sleep=lambda _: None)

    def test_workflow_uses_resumable_publisher_and_runs_its_tests(self):
        root = Path(__file__).resolve().parent.parent
        workflow = (root / ".github/workflows/publish.yml").read_text()
        run_lines = [line.strip() for line in workflow.splitlines()]
        self.assertIn("run: python3 scripts/publish-crates.py --dry-run", run_lines)
        self.assertIn("run: python3 scripts/publish-crates.py", run_lines)
        self.assertNotIn("cargo publish --locked -p", workflow)
        self.assertIn("python3 -m unittest discover -s scripts -p 'test_*.py'",
                      (root / ".github/workflows/ci.yml").read_text())


if __name__ == "__main__":
    unittest.main()
