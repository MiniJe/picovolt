#!/usr/bin/env python3
"""Validate and run PicoVolt starters without checkout-local dependencies.

``policy`` is fast and network-free, so it runs on every change. ``run`` copies
selected starters into a temporary directory, installs their exact release from
the public package service, verifies the resolved package origin, and executes
the starter. Release workflows use ``run`` after publishing.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Dict, Iterable, Mapping, Optional, Sequence


ROOT = Path(__file__).resolve().parents[1]
GO_MODULE = "github.com/MiniJe/picovolt/bindings/go"
STARTER_NAMES = ("rust", "node", "browser", "python", "go")
CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
_SAFE_ENVIRONMENT_NAMES = (
    "COMSPEC",
    "LANG",
    "LC_ALL",
    "PATH",
    "PATHEXT",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "SYSTEMROOT",
    "TERM",
    "WINDIR",
)
_GENERATED_STARTER_NAMES = (
    ".pytest_cache",
    ".venv",
    "Cargo.lock",
    "__pycache__",
    "dist",
    "node_modules",
    "target",
    "*.pv",
    "*.pvdb",
    "*.pyc",
)


class PolicyError(RuntimeError):
    """A starter can resolve code outside a public package service."""


def project_version(root: Path = ROOT) -> str:
    manifest = (root / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"\s*$', manifest)
    if not match:
        raise PolicyError("Cargo.toml has no package version")
    return match.group(1)


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise PolicyError(message)


def _npm_policy(root: Path, name: str, version: str) -> None:
    directory = root / "starters" / name
    package = json.loads((directory / "package.json").read_text(encoding="utf-8"))
    declared = package.get("dependencies", {}).get("picovolt")
    _require(
        declared == version,
        f"{name}: picovolt must be pinned to {version}, found {declared!r}",
    )
    for group in ("dependencies", "devDependencies", "optionalDependencies"):
        for dependency, specifier in package.get(group, {}).items():
            lowered = str(specifier).lower()
            _require(
                not lowered.startswith(("file:", "link:", "workspace:", "git+", "github:"))
                and "../" not in lowered
                and "\\" not in lowered,
                f"{name}: {dependency} uses a non-registry {group} specifier",
            )

    lock_path = directory / "package-lock.json"
    _require(lock_path.is_file(), f"{name}: package-lock.json is required")
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    root_declared = lock.get("packages", {}).get("", {}).get("dependencies", {}).get("picovolt")
    installed = lock.get("packages", {}).get("node_modules/picovolt", {})
    _require(root_declared == version, f"{name}: lockfile root dependency is not {version}")
    _require(installed.get("version") == version, f"{name}: lockfile resolves another version")
    resolved = installed.get("resolved", "")
    _require(
        resolved.startswith("https://registry.npmjs.org/picovolt/-/"),
        f"{name}: lockfile does not resolve picovolt from registry.npmjs.org",
    )
    for package_name, locked in lock.get("packages", {}).items():
        _require(not locked.get("link", False), f"{name}: lockfile contains link {package_name}")
        locked_source = locked.get("resolved")
        if locked_source:
            _require(
                locked_source.startswith("https://registry.npmjs.org/"),
                f"{name}: lockfile package {package_name} is not from registry.npmjs.org",
            )
            integrity = locked.get("integrity")
            _require(
                isinstance(integrity, str) and integrity.startswith("sha512-"),
                f"{name}: lockfile package {package_name} has no SHA-512 integrity pin",
            )


def check_policy(root: Path = ROOT, version: Optional[str] = None) -> None:
    """Raise ``PolicyError`` unless every starter is registry-only and pinned."""

    version = version or project_version(root)

    python_project = (root / "bindings/python/pyproject.toml").read_text(
        encoding="utf-8"
    )
    python_project_version = re.search(
        r'(?m)^version\s*=\s*"([^"]+)"\s*$', python_project
    )
    _require(
        bool(python_project_version),
        "python: bindings/python/pyproject.toml has no project version",
    )
    _require(
        python_project_version.group(1) == version,
        f"python: distribution version must be {version}",
    )

    python_module = (root / "bindings/python/picovolt/__init__.py").read_text(
        encoding="utf-8"
    )
    python_module_version = re.search(
        r'(?m)^__version__\s*=\s*"([^"]+)"\s*$', python_module
    )
    _require(bool(python_module_version), "python: __version__ is missing")
    _require(
        python_module_version.group(1) == version,
        f"python: module version must be {version}",
    )

    canonical_header = (root / "include/picovolt.h").read_bytes()
    go_header = (root / "bindings/go/include/picovolt.h").read_bytes()
    _require(
        go_header == canonical_header,
        "go: copied C header differs from include/picovolt.h",
    )

    cargo = (root / "starters/rust-cli/Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'(?m)^picovolt\s*=\s*"([^"]+)"\s*$', cargo)
    _require(bool(match), "rust: picovolt must use a simple registry version")
    _require(match.group(1) == f"={version}", f"rust: picovolt must be pinned to ={version}")
    _require(not re.search(r"(?m)\b(path|git|workspace)\s*=", cargo), "rust: local/git dependency found")

    _npm_policy(root, "node", version)
    _npm_policy(root, "browser", version)

    requirements = (root / "starters/python/requirements.txt").read_text(encoding="utf-8")
    lines = [line.strip() for line in requirements.splitlines() if line.strip() and not line.lstrip().startswith("#")]
    _require(lines == [f"picovolt=={version}"], f"python: requirements must contain only picovolt=={version}")
    lowered = requirements.lower()
    _require(
        not any(marker in lowered for marker in ("-e ", "file:", "git+", "../", "\\")),
        "python: editable, VCS, or filesystem requirement found",
    )

    go_mod = (root / "starters/go/go.mod").read_text(encoding="utf-8")
    go_requirement = re.search(
        rf"(?m)^\s*require\s+{re.escape(GO_MODULE)}\s+(v\S+)\s*$", go_mod
    )
    _require(bool(go_requirement), "go: public PicoVolt module requirement is missing")
    _require(go_requirement.group(1) == f"v{version}", f"go: module must be pinned to v{version}")
    _require(not re.search(r"(?m)^\s*replace\b", go_mod), "go: replace directives are forbidden")

    go_sum_path = root / "starters/go/go.sum"
    _require(go_sum_path.is_file(), "go: go.sum is required")
    go_sum = go_sum_path.read_text(encoding="utf-8").splitlines()
    module_prefix = f"{GO_MODULE} v{version}"
    _require(
        any(line.startswith(f"{module_prefix} h1:") for line in go_sum),
        f"go: go.sum has no checksum for {module_prefix}",
    )
    _require(
        any(line.startswith(f"{module_prefix}/go.mod h1:") for line in go_sum),
        f"go: go.sum has no go.mod checksum for {module_prefix}",
    )


def _run(
    command: Sequence[str],
    *,
    cwd: Path,
    env: Optional[Dict[str, str]] = None,
    capture: bool = False,
    timeout: int = 300,
) -> subprocess.CompletedProcess[str]:
    print(f"+ {' '.join(command)}  (in {cwd})", flush=True)
    return subprocess.run(
        command,
        cwd=cwd,
        env=env,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        timeout=timeout,
    )


def _retry(
    action,
    label: str,
    attempts: int = 12,
    delay: int = 15,
    max_elapsed: int = 600,
):
    _require(attempts > 0, "retry attempts must be positive")
    _require(delay >= 0, "retry delay cannot be negative")
    _require(max_elapsed > 0, "retry wall-clock limit must be positive")
    started = time.monotonic()
    for attempt in range(1, attempts + 1):
        try:
            return action()
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
            elapsed = time.monotonic() - started
            if attempt == attempts or elapsed + delay >= max_elapsed:
                raise
            print(
                f"{label} is not available yet; "
                f"attempt {attempt}/{attempts} failed, retrying in {delay}s",
                flush=True,
            )
            time.sleep(delay)


def _copy_starter(source_name: str, destination: Path) -> Path:
    source = ROOT / "starters" / source_name
    target = destination / source_name
    shutil.copytree(
        source,
        target,
        symlinks=True,
        ignore=shutil.ignore_patterns(*_GENERATED_STARTER_NAMES),
    )
    for path in target.rglob("*"):
        _require(not path.is_symlink(), f"{source_name}: starter contains symlink {path}")
    return target


def _registry_environment(workspace: Path) -> Dict[str, str]:
    """Return a minimal process environment for a public-registry smoke test.

    In particular, this drops package-manager config, source replacement,
    language search paths, loader overrides, and credentials inherited from the
    runner. Toolchain discovery still works through PATH (and RUSTUP_HOME when
    rustup is installed outside its default location).
    """

    environment = {
        name: os.environ[name]
        for name in _SAFE_ENVIRONMENT_NAMES
        if name in os.environ
    }
    original_home = Path.home()
    rustup_home = os.environ.get("RUSTUP_HOME", str(original_home / ".rustup"))
    if Path(rustup_home).is_dir():
        environment["RUSTUP_HOME"] = rustup_home

    home = workspace / "home"
    temporary = workspace / "tmp"
    home.mkdir(parents=True, exist_ok=True)
    temporary.mkdir(parents=True, exist_ok=True)
    environment.update(
        {
            "CI": "true",
            "HOME": str(home),
            "TEMP": str(temporary),
            "TMP": str(temporary),
            "TMPDIR": str(temporary),
        }
    )
    if os.name == "nt":
        app_data = home / "AppData" / "Roaming"
        local_app_data = home / "AppData" / "Local"
        app_data.mkdir(parents=True, exist_ok=True)
        local_app_data.mkdir(parents=True, exist_ok=True)
        environment.update(
            {
                "APPDATA": str(app_data),
                "LOCALAPPDATA": str(local_app_data),
                "USERPROFILE": str(home),
            }
        )
    return environment


def _require_crates_io_package(package: Mapping[str, object], version: str) -> None:
    _require(package.get("version") == version, "rust: Cargo resolved the wrong version")
    _require(
        package.get("source") == CRATES_IO_SOURCE,
        "rust: Cargo did not resolve picovolt from the canonical crates.io index",
    )


def _run_rust(temp: Path, version: str) -> None:
    starter = _copy_starter("rust-cli", temp)
    env = _registry_environment(temp / "rust-environment")
    env["CARGO_HOME"] = str(temp / "cargo-home")
    env["CARGO_REGISTRIES_CRATES_IO_PROTOCOL"] = "sparse"
    metadata = _retry(
        lambda: _run(
            ["cargo", "metadata", "--format-version", "1"],
            cwd=starter,
            env=env,
            capture=True,
            timeout=180,
        ),
        "crates.io package",
    )
    packages = json.loads(metadata.stdout)["packages"]
    package = next((item for item in packages if item["name"] == "picovolt"), None)
    _require(package is not None, "rust: cargo metadata omitted picovolt")
    _require_crates_io_package(package, version)
    _run(["cargo", "run", "--quiet", "--locked"], cwd=starter, env=env)


def _run_npm(temp: Path, name: str, version: str) -> None:
    starter = _copy_starter(name, temp)
    env = _registry_environment(temp / f"{name}-environment")
    env["npm_config_cache"] = str(temp / "npm-cache")
    env["npm_config_fund"] = "false"
    env["npm_config_audit"] = "false"
    env["npm_config_ignore_scripts"] = "true"
    env["npm_config_registry"] = "https://registry.npmjs.org"
    install = lambda: _run(
        ["npm", "ci", "--registry=https://registry.npmjs.org", "--ignore-scripts"],
        cwd=starter,
        env=env,
        timeout=180,
    )
    _retry(install, "npm package")
    listing = _run(["npm", "ls", "picovolt", "--json"], cwd=starter, env=env, capture=True)
    resolved = json.loads(listing.stdout)["dependencies"]["picovolt"]
    _require(resolved["version"] == version, f"{name}: npm resolved the wrong version")
    if name == "node":
        _run(["npm", "start"], cwd=starter, env=env)
    else:
        _run(["npm", "run", "build"], cwd=starter, env=env)
        _run(
            ["node", "--experimental-wasm-modules", "smoke.mjs"],
            cwd=starter,
            env=env,
        )


def _venv_python(venv: Path) -> Path:
    if os.name == "nt":
        return venv / "Scripts/python.exe"
    return venv / "bin/python"


def _public_pypi_environment(workspace: Path) -> Dict[str, str]:
    env = _registry_environment(workspace)
    env.update(
        {
            "PIP_CONFIG_FILE": os.devnull,
            "PIP_DISABLE_PIP_VERSION_CHECK": "1",
            "PIP_INDEX_URL": "https://pypi.org/simple",
            "PIP_NO_CACHE_DIR": "1",
            "PIP_NO_INPUT": "1",
            "PYTHONDONTWRITEBYTECODE": "1",
            "PYTHONNOUSERSITE": "1",
        }
    )
    return env


def _probe_python_install(python: Path, venv: Path, version: str, starter: Path, env: Dict[str, str]) -> None:
    probe = r"""
import importlib.metadata
import json
import os
from pathlib import Path
import sys

import picovolt

expected = sys.argv[1]
venv = Path(sys.argv[2]).resolve(strict=True)
module = Path(picovolt.__file__).absolute()
module_resolved = module.resolve(strict=True)
try:
    inside_venv = os.path.commonpath((str(module_resolved), str(venv))) == str(venv)
except ValueError:
    inside_venv = False
if not inside_venv or module.is_symlink():
    raise RuntimeError(f"picovolt module is not a regular file in the clean venv: {module}")

package_dir = module_resolved.parent
native_name = {
    "win32": "picovolt.dll",
    "darwin": "libpicovolt.dylib",
}.get(sys.platform, "libpicovolt.so")
native = package_dir / native_name
if not native.is_file() or native.is_symlink():
    raise RuntimeError(f"wheel does not contain a regular, non-symlink native library: {native}")
loaded = Path(str(picovolt._lib._name)).absolute()
if loaded.resolve(strict=True) != native.resolve(strict=True):
    raise RuntimeError(f"ctypes loaded {loaded}, expected the wheel library {native}")

distribution_version = importlib.metadata.version("picovolt")
native_version = picovolt.version()
if (distribution_version, picovolt.__version__, native_version) != (expected, expected, expected):
    raise RuntimeError(
        "version mismatch: "
        f"metadata={distribution_version}, module={picovolt.__version__}, "
        f"pv_version={native_version}, expected={expected}"
    )
print(json.dumps({"module": str(module_resolved), "native": str(native), "version": native_version}))
"""
    result = _run(
        [str(python), "-c", probe, version, str(venv)],
        cwd=starter,
        env=env,
        capture=True,
    )
    details = json.loads(result.stdout)
    _require(details.get("version") == version, "python: installed native library reported another version")


def _run_python(temp: Path, version: str) -> None:
    starter = _copy_starter("python", temp)
    venv = temp / "python-venv"
    pip_env = _public_pypi_environment(temp / "python-environment")
    _run([sys.executable, "-m", "venv", str(venv)], cwd=starter, env=pip_env)
    python = _venv_python(venv)
    install = lambda: _run(
        [
            str(python),
            "-m",
            "pip",
            "install",
            "--only-binary=:all:",
            "--no-deps",
            "--index-url=https://pypi.org/simple",
            "-r",
            "requirements.txt",
        ],
        cwd=starter,
        env=pip_env,
        timeout=180,
    )
    _retry(install, "PyPI wheel", attempts=20)
    _probe_python_install(python, venv, version, starter, pip_env)
    _run([str(python), "app.py"], cwd=starter, env=pip_env)
    _run([str(python), "app.py"], cwd=starter, env=pip_env)


def _run_go(temp: Path, version: str) -> None:
    if not sys.platform.startswith("linux"):
        raise PolicyError("go: the clean native-library gate currently runs on Linux")
    starter = _copy_starter("go", temp)

    # The Go wrapper is public on proxy.golang.org. Its cgo ABI is paired with
    # the exact native library bundled in PicoVolt's public manylinux wheel.
    native = temp / "go-native"
    pip_env = _public_pypi_environment(temp / "go-python-environment")
    install_native = lambda: _run(
        [
            sys.executable,
            "-m",
            "pip",
            "install",
            "--only-binary=:all:",
            "--no-deps",
            "--target",
            str(native),
            "--index-url=https://pypi.org/simple",
            f"picovolt=={version}",
        ],
        cwd=starter,
        env=pip_env,
        timeout=180,
    )
    _retry(install_native, "PyPI native library", attempts=20)
    library = native / "picovolt" / "libpicovolt.so"
    _require(
        library.is_file() and not library.is_symlink(),
        "go: PyPI wheel did not contain picovolt/libpicovolt.so as a regular file",
    )
    probe = r"""
import ctypes
from pathlib import Path
import sys

library = Path(sys.argv[1]).absolute()
expected = sys.argv[2]
if not library.is_file() or library.is_symlink():
    raise RuntimeError(f"native library is not a regular, non-symlink file: {library}")
loaded = ctypes.CDLL(str(library))
loaded.pv_version.restype = ctypes.c_char_p
actual = loaded.pv_version().decode("utf-8")
if actual != expected:
    raise RuntimeError(f"native pv_version is {actual}, expected {expected}")
print(actual)
"""
    native_version = _run(
        [sys.executable, "-c", probe, str(library), version],
        cwd=starter,
        env=pip_env,
        capture=True,
    )
    _require(native_version.stdout.strip() == version, "go: native pv_version mismatch")
    library_dir = library.parent

    env = _registry_environment(temp / "go-environment")
    env.update(
        {
            "CGO_ENABLED": "1",
            "CGO_LDFLAGS": f"-L{library_dir}",
            "GOENV": "off",
            "GOPROXY": "https://proxy.golang.org",
            "GOSUMDB": "sum.golang.org",
            "GOPRIVATE": "",
            "GONOPROXY": "",
            "GONOSUMDB": "",
            "GOTOOLCHAIN": "local",
            "GOVCS": "*:off",
            "GOWORK": "off",
            "GOMODCACHE": str(temp / "go-mod-cache"),
            "GOCACHE": str(temp / "go-build-cache"),
            "LD_LIBRARY_PATH": str(library_dir),
        }
    )
    module = f"{GO_MODULE}@v{version}"
    _retry(
        lambda: _run(
            ["go", "mod", "download", module],
            cwd=starter,
            env=env,
            timeout=180,
        ),
        "Go module",
    )
    details = _run(["go", "list", "-m", "-json", GO_MODULE], cwd=starter, env=env, capture=True)
    resolved = json.loads(details.stdout)
    _require(resolved["Version"] == f"v{version}", "go: module proxy resolved the wrong version")
    _require("Replace" not in resolved, "go: module resolution used a replacement")
    _run(["go", "run", "."], cwd=starter, env=env)


def run_starters(starters: Iterable[str], version: str) -> None:
    check_policy(ROOT, version)
    with tempfile.TemporaryDirectory(prefix="picovolt-registry-starters-") as directory:
        temp = Path(directory)
        for starter in starters:
            print(f"\n== {starter} starter ==", flush=True)
            if starter == "rust":
                _run_rust(temp, version)
            elif starter in ("node", "browser"):
                _run_npm(temp, starter, version)
            elif starter == "python":
                _run_python(temp, version)
            elif starter == "go":
                _run_go(temp, version)
            else:  # argparse constrains this; retain a safe programmatic error.
                raise PolicyError(f"unknown starter {starter!r}")


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("policy", "run"))
    parser.add_argument("--version", help="release version; defaults to Cargo.toml")
    parser.add_argument(
        "--starter",
        action="append",
        choices=STARTER_NAMES,
        dest="starters",
        help="starter to execute in run mode (repeatable; default: all)",
    )
    args = parser.parse_args(argv)
    version = args.version or project_version()
    try:
        check_policy(ROOT, version)
        print(f"starter policy passed for PicoVolt {version}")
        if args.mode == "run":
            run_starters(args.starters or STARTER_NAMES, version)
    except (PolicyError, OSError, subprocess.SubprocessError) as error:
        print(f"starter gate failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
