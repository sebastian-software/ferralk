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
        with patch.object(publisher.urllib.request, "urlopen", return_value=io.BytesIO(payload)):
            self.assertEqual(publisher.registry_version("ferralk", "1.2.0"), PUBLISHED)

    def test_workflow_uses_resumable_publisher_and_runs_its_tests(self):
        root = Path(__file__).resolve().parent.parent
        workflow = (root / ".github/workflows/publish.yml").read_text()
        self.assertIn("python3 scripts/publish-crates.py", workflow)
        self.assertNotIn("cargo publish --locked -p", workflow)
        self.assertIn("python3 -m unittest discover -s scripts -p 'test_*.py'",
                      (root / ".github/workflows/ci.yml").read_text())


if __name__ == "__main__":
    unittest.main()
