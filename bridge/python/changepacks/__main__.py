from __future__ import annotations

import os
import sys
import sysconfig
from importlib import resources


def get_last_three_path_parts(path: str) -> list[str]:
    """Return a list of up to the last three parts of a path."""
    parts: list[str] = []

    while len(parts) < 3:
        head, tail = os.path.split(path)
        if tail or head != path:
            parts.append(tail)
            path = head
        else:
            parts.append(path)
            break

    parts.reverse()
    return parts


def changepacks_exe_names() -> list[str]:
    """Return platform-specific executable names to probe."""
    configured_suffix = sysconfig.get_config_var("EXE") or ""
    names = ["changepacks" + configured_suffix]
    if os.name == "nt" and configured_suffix.lower() != ".exe":
        names.append("changepacks.exe")
    return names


def user_scripts_path() -> str:
    """Return the preferred per-user scripts directory for this interpreter."""

    if sys.version_info >= (3, 10):
        user_scheme = sysconfig.get_preferred_scheme("user")
    elif os.name == "nt":
        user_scheme = "nt_user"
    elif sys.platform == "darwin" and getattr(sys, "_framework", None):
        user_scheme = "osx_framework_user"
    else:
        user_scheme = "posix_user"

    return sysconfig.get_path("scripts", scheme=user_scheme)


def changepacks_bin_candidates() -> list[str]:
    """Return executable paths supported by common wheel and target layouts."""
    package_dir = os.path.dirname(__file__)
    pkg_root = os.path.dirname(package_dir)
    exe_names = changepacks_exe_names()
    search_dirs = [
        sysconfig.get_path("scripts"),
        user_scripts_path(),
        package_dir,
        pkg_root,
        os.path.join(pkg_root, "bin"),
        os.path.join(pkg_root, "Scripts"),
    ]

    resource_candidates: list[str] = []
    if __package__:
        package_files = resources.files(__package__)
        for exe_name in exe_names:
            resource_candidates.append(str(package_files.joinpath(exe_name)))

    paths: list[str] = []
    seen: set[str] = set()
    candidates = [
        os.path.join(directory, exe_name)
        for directory in search_dirs
        for exe_name in exe_names
    ]
    candidates.extend(resource_candidates)
    for candidate in candidates:
        if candidate not in seen:
            seen.add(candidate)
            paths.append(candidate)
    return paths


def is_eligible_changepacks_bin(path: str) -> bool:
    """Return whether a candidate is a runnable platform binary."""
    return os.path.isfile(path) and (os.name == "nt" or os.access(path, os.X_OK))


def find_changepacks_bin() -> str:
    """Return the changepacks binary path. (ruff code)"""

    candidates = changepacks_bin_candidates()
    for candidate_path in candidates:
        if is_eligible_changepacks_bin(candidate_path):
            return candidate_path

    # Search for pip-specific build environments.
    #
    # Expect to find changepacks in <prefix>/pip-build-env-<rand>/overlay/bin/changepacks
    # Expect to find a "normal" folder at <prefix>/pip-build-env-<rand>/normal
    #
    # See: https://github.com/pypa/pip/blob/102d8187a1f5a4cd5de7a549fd8a9af34e89a54f/src/pip/_internal/build_env.py#L87
    scripts_name = "Scripts" if os.name == "nt" else "bin"
    build_env_entries: list[tuple[str, str, str]] = []
    for path in os.environ.get("PATH", "").split(os.pathsep):
        scripts_path = os.path.normpath(path)
        parts = get_last_three_path_parts(scripts_path)
        if len(parts) != 3:
            continue

        build_env_name, layer, maybe_scripts_name = parts
        if (
            maybe_scripts_name != scripts_name
            or layer not in ("overlay", "normal")
            or not build_env_name.startswith("pip-build-env-")
        ):
            continue

        build_env_path = os.path.normcase(
            os.path.abspath(os.path.dirname(os.path.dirname(scripts_path)))
        )
        build_env_entries.append((scripts_path, layer, build_env_path))

    layers_by_build_env: dict[str, set[str]] = {}
    for _, layer, build_env_path in build_env_entries:
        layers_by_build_env.setdefault(build_env_path, set()).add(layer)

    for scripts_path, layer, build_env_path in build_env_entries:
        if layer != "overlay" or "normal" not in layers_by_build_env[build_env_path]:
            continue

        # The overlay must contain the changepacks binary.
        for exe_name in changepacks_exe_names():
            candidate = os.path.join(scripts_path, exe_name)
            candidates.append(candidate)
            if is_eligible_changepacks_bin(candidate):
                return candidate

    raise FileNotFoundError(
        "Could not find the changepacks executable. Searched: "
        + os.pathsep.join(candidates)
    )


if __name__ == "__main__":
    changepacks = find_changepacks_bin()
    if sys.platform == "win32":
        import subprocess

        completed_process = subprocess.run([changepacks, *sys.argv[1:]])
        sys.exit(completed_process.returncode)
    else:
        os.execvp(changepacks, [changepacks, *sys.argv[1:]])
