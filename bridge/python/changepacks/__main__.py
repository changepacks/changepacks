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
    search_dirs = [
        sysconfig.get_path("scripts"),
        user_scripts_path(),
        package_dir,
        pkg_root,
        os.path.join(pkg_root, "bin"),
        os.path.join(pkg_root, "Scripts"),
    ]

    if __package__:
        package_files = resources.files(__package__)
        for exe_name in changepacks_exe_names():
            search_dirs.append(str(package_files.joinpath(exe_name)))

    paths: list[str] = []
    seen: set[str] = set()
    for candidate in search_dirs:
        for exe_name in changepacks_exe_names():
            path = candidate if candidate.endswith(exe_name) else os.path.join(candidate, exe_name)
            if path not in seen:
                seen.add(path)
                paths.append(path)
    return paths


def find_changepacks_bin() -> str:
    """Return the changepacks binary path. (ruff code)"""

    candidates = changepacks_bin_candidates()
    for candidate_path in candidates:
        if os.path.isfile(candidate_path):
            return candidate_path

    # Search for pip-specific build environments.
    #
    # Expect to find changepacks in <prefix>/pip-build-env-<rand>/overlay/bin/changepacks
    # Expect to find a "normal" folder at <prefix>/pip-build-env-<rand>/normal
    #
    # See: https://github.com/pypa/pip/blob/102d8187a1f5a4cd5de7a549fd8a9af34e89a54f/src/pip/_internal/build_env.py#L87
    paths = os.environ.get("PATH", "").split(os.pathsep)
    if len(paths) >= 2:
        maybe_overlay = get_last_three_path_parts(paths[0])
        maybe_normal = get_last_three_path_parts(paths[1])
        if (
            len(maybe_normal) >= 3
            and maybe_normal[0] == "normal"
            and maybe_normal[1].startswith("pip-build-env-")
            and len(maybe_overlay) >= 3
            and maybe_overlay[0] == "overlay"
            and maybe_overlay[1].startswith("pip-build-env-")
        ):
            # The overlay must contain the changepacks binary.
            for exe_name in changepacks_exe_names():
                candidate = os.path.join(paths[0], exe_name)
                candidates.append(candidate)
                if os.path.isfile(candidate):
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
