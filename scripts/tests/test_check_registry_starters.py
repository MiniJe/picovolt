import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

from scripts.check_registry_starters import (
    CRATES_IO_SOURCE,
    PolicyError,
    ROOT,
    _public_pypi_environment,
    _registry_environment,
    _require_crates_io_package,
    _retry,
    check_policy,
    project_version,
)


class StarterPolicyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="picovolt-starter-policy-")
        self.root = Path(self.temp.name)
        shutil.copy2(ROOT / "Cargo.toml", self.root / "Cargo.toml")
        (self.root / "bindings/python/picovolt").mkdir(parents=True)
        shutil.copy2(
            ROOT / "bindings/python/pyproject.toml",
            self.root / "bindings/python/pyproject.toml",
        )
        shutil.copy2(
            ROOT / "bindings/python/picovolt/__init__.py",
            self.root / "bindings/python/picovolt/__init__.py",
        )
        (self.root / "include").mkdir()
        shutil.copy2(ROOT / "include/picovolt.h", self.root / "include/picovolt.h")
        (self.root / "bindings/go/include").mkdir(parents=True)
        shutil.copy2(
            ROOT / "bindings/go/include/picovolt.h",
            self.root / "bindings/go/include/picovolt.h",
        )
        shutil.copytree(
            ROOT / "starters",
            self.root / "starters",
            ignore=shutil.ignore_patterns("node_modules", "dist", "*.pv", "*.pvdb"),
        )

    def tearDown(self):
        self.temp.cleanup()

    def assert_rejected(self, message):
        with self.assertRaisesRegex(PolicyError, message):
            check_policy(self.root)

    def test_current_starters_are_registry_only(self):
        check_policy(self.root)

    def test_rust_path_dependency_is_rejected(self):
        manifest = self.root / "starters/rust-cli/Cargo.toml"
        version = project_version(self.root)
        manifest.write_text(
            manifest.read_text().replace(
                f'picovolt = "={version}"', 'picovolt = { path = "../.." }'
            )
        )
        self.assert_rejected("rust")

    def test_npm_file_dependency_is_rejected(self):
        manifest = self.root / "starters/node/package.json"
        version = project_version(self.root)
        manifest.write_text(
            manifest.read_text().replace(
                f'"picovolt": "{version}"', '"picovolt": "file:../.."'
            )
        )
        self.assert_rejected("node")

    def test_npm_file_dev_dependency_is_rejected(self):
        manifest = self.root / "starters/browser/package.json"
        package = json.loads(manifest.read_text())
        package["devDependencies"]["vite"] = "file:../../vite"
        manifest.write_text(json.dumps(package))
        self.assert_rejected("browser")

    def test_npm_non_registry_lock_resolution_is_rejected(self):
        lockfile = self.root / "starters/node/package-lock.json"
        lockfile.write_text(
            lockfile.read_text().replace(
                "https://registry.npmjs.org/picovolt/-/", "file:../../packages/"
            )
        )
        self.assert_rejected("lockfile")

    def test_npm_lock_entry_without_integrity_is_rejected(self):
        lockfile = self.root / "starters/node/package-lock.json"
        lock = json.loads(lockfile.read_text())
        del lock["packages"]["node_modules/picovolt"]["integrity"]
        lockfile.write_text(json.dumps(lock))
        self.assert_rejected("integrity")

    def test_python_editable_dependency_is_rejected(self):
        requirements = self.root / "starters/python/requirements.txt"
        requirements.write_text("-e ../..\n")
        self.assert_rejected("python")

    def test_go_replace_is_rejected(self):
        go_mod = self.root / "starters/go/go.mod"
        with go_mod.open("a") as handle:
            handle.write("\nreplace github.com/MiniJe/picovolt/bindings/go => ../../bindings/go\n")
        self.assert_rejected("go")

    def test_go_sum_is_required(self):
        (self.root / "starters/go/go.sum").unlink()
        self.assert_rejected("go.sum is required")

    def test_go_sum_must_pin_the_release(self):
        go_sum = self.root / "starters/go/go.sum"
        version = project_version(self.root)
        go_sum.write_text(
            go_sum.read_text().replace(f" v{version} ", " v9.9.9 ")
        )
        self.assert_rejected("no checksum")

    def test_version_mismatch_is_rejected(self):
        with self.assertRaises(PolicyError):
            check_policy(self.root, "9.9.9")

    def test_python_distribution_version_mismatch_is_rejected(self):
        pyproject = self.root / "bindings/python/pyproject.toml"
        version = project_version(self.root)
        pyproject.write_text(
            pyproject.read_text().replace(
                f'version = "{version}"', 'version = "9.9.9"'
            )
        )
        self.assert_rejected("distribution version")

    def test_python_module_version_mismatch_is_rejected(self):
        module = self.root / "bindings/python/picovolt/__init__.py"
        version = project_version(self.root)
        module.write_text(
            module.read_text().replace(
                f'__version__ = "{version}"', '__version__ = "9.9.9"'
            )
        )
        self.assert_rejected("module version")

    def test_go_c_header_mismatch_is_rejected(self):
        header = self.root / "bindings/go/include/picovolt.h"
        header.write_bytes(header.read_bytes() + b"\n/* stale copy */\n")
        self.assert_rejected("copied C header")

    def test_registry_environment_drops_manager_overrides_and_credentials(self):
        poisoned = {
            "CARGO_REGISTRIES_CRATES_IO_INDEX": "https://example.invalid/cargo",
            "GITHUB_TOKEN": "secret",
            "LD_LIBRARY_PATH": "/checkout/target/release",
            "NODE_OPTIONS": "--require=/checkout/hook.js",
            "npm_config_registry": "https://example.invalid/npm",
            "PIP_EXTRA_INDEX_URL": "https://example.invalid/pypi",
            "PICOVOLT_LIB": "/checkout/target/release/libpicovolt.so",
            "PYTHONPATH": "/checkout/bindings/python",
        }
        with mock.patch.dict(os.environ, poisoned):
            environment = _registry_environment(self.root / "clean-environment")
        for name in poisoned:
            self.assertNotIn(name, environment)
        self.assertEqual(environment["CI"], "true")
        self.assertNotEqual(environment["HOME"], os.environ.get("HOME"))

    def test_pypi_environment_has_one_explicit_public_index(self):
        with mock.patch.dict(
            os.environ,
            {
                "PIP_INDEX_URL": "https://example.invalid/simple",
                "PIP_EXTRA_INDEX_URL": "https://other.invalid/simple",
            },
        ):
            environment = _public_pypi_environment(self.root / "pypi-environment")
        self.assertEqual(environment["PIP_INDEX_URL"], "https://pypi.org/simple")
        self.assertNotIn("PIP_EXTRA_INDEX_URL", environment)
        self.assertEqual(environment["PIP_CONFIG_FILE"], os.devnull)

    def test_crate_origin_must_be_the_canonical_crates_io_index(self):
        package = {"version": project_version(self.root), "source": CRATES_IO_SOURCE}
        _require_crates_io_package(package, project_version(self.root))
        package["source"] = "registry+https://example.invalid/index"
        with self.assertRaisesRegex(PolicyError, "canonical crates.io"):
            _require_crates_io_package(package, project_version(self.root))

    def test_registry_retry_is_bounded(self):
        attempts = 0

        def fail():
            nonlocal attempts
            attempts += 1
            raise subprocess.CalledProcessError(1, ["registry-probe"])

        with mock.patch("scripts.check_registry_starters.time.sleep") as sleep:
            with self.assertRaises(subprocess.CalledProcessError):
                _retry(fail, "registry", attempts=3, delay=0)
        self.assertEqual(attempts, 3)
        self.assertEqual(sleep.call_count, 2)

    def test_registry_retry_stops_at_wall_clock_limit(self):
        attempts = 0

        def fail():
            nonlocal attempts
            attempts += 1
            raise subprocess.TimeoutExpired(["registry-probe"], 1)

        with mock.patch(
            "scripts.check_registry_starters.time.monotonic", side_effect=(0, 11)
        ), mock.patch("scripts.check_registry_starters.time.sleep") as sleep:
            with self.assertRaises(subprocess.TimeoutExpired):
                _retry(fail, "registry", attempts=10, delay=1, max_elapsed=10)
        self.assertEqual(attempts, 1)
        sleep.assert_not_called()


if __name__ == "__main__":
    unittest.main()
