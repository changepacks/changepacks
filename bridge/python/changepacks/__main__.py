from __future__ import annotations

import os
import sys
import sysconfig


def find_changepacks_bin() -> str:
    """Return the changepacks binary path. (ruff code)"""

    changepacks_exe = "changepacks" + sysconfig.get_config_var("EXE")

    scripts_path = os.path.join(sysconfig.get_path("scripts"), changepacks_exe)
    if os.path.isfile(scripts_path):
        return scripts_path

    if sys.version_info >= (3, 10):
        user_scheme = sysconfig.get_preferred_scheme("user")
    elif os.name == "nt":
        user_scheme = "nt_user"
    elif sys.platform == "darwin" and sys._framework:
        user_scheme = "osx_framework_user"
    else:
        user_scheme = "posix_user"

    user_path = os.path.join(
        sysconfig.get_path("scripts", scheme=user_scheme), changepacks_exe
    )
    if os.path.isfile(user_path):
        return user_path

    # Search in `bin` adjacent to package root (as created by `pip install --target`).
    pkg_root = os.path.dirname(os.path.dirname(__file__))
    target_path = os.path.join(pkg_root, "bin", changepacks_exe)
    if os.path.isfile(target_path):
        return target_path

    # Search for pip-specific build environments.
    #
    # Expect to find changepacks in <prefix>/pip-build-env-<rand>/overlay/bin/changepacks
    # Expect to find a "normal" folder at <prefix>/pip-build-env-<rand>/normal
    #
    # See: https://github.com/pypa/pip/blob/102d8187a1f5a4cd5de7a549fd8a9af34e89a54f/src/pip/_internal/build_env.py#L87
    paths = os.environ.get("PATH", "").split(os.pathsep)
    if len(paths) >= 2:

        def get_last_three_path_parts(path: str) -> list[str]:
            """Return a list of up to the last three parts of a path."""
            parts = []

            while len(parts) < 3:
                head, tail = os.path.split(path)
                if tail or head != path:
                    parts.append(tail)
                    path = head
                else:
                    parts.append(path)
                    break

            return parts

        maybe_overlay = get_last_three_path_parts(paths[0])
        maybe_normal = get_last_three_path_parts(paths[1])
        if (
            len(maybe_normal) >= 3
            and maybe_normal[-1].startswith("pip-build-env-")
            and maybe_normal[-2] == "normal"
            and len(maybe_overlay) >= 3
            and maybe_overlay[-1].startswith("pip-build-env-")
            and maybe_overlay[-2] == "overlay"
        ):
            # The overlay must contain the changepacks binary.
            candidate = os.path.join(paths[0], changepacks_exe)
            if os.path.isfile(candidate):
                return candidate

    raise FileNotFoundError(scripts_path)


if __name__ == "__main__":
    changepacks = find_changepacks_bin()
    if sys.platform == "win32":
        import subprocess

        completed_process = subprocess.run([changepacks, *sys.argv[1:]])
        sys.exit(completed_process.returncode)
    else:
        os.execvp(changepacks, [changepacks, *sys.argv[1:]])