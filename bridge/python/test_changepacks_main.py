import os
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))

from changepacks import __main__ as launcher


class _ResourceDirectory:
    def __init__(self, paths):
        self.paths = paths

    def joinpath(self, name):
        return self.paths[name]


class LastThreePathPartsTests(unittest.TestCase):
    def test_returns_path_tail_in_natural_order(self):
        path = os.path.join("tmp", "pip-build-env-demo", "overlay", "bin")

        self.assertEqual(
            launcher.get_last_three_path_parts(path),
            ["pip-build-env-demo", "overlay", "bin"],
        )


class ChangepacksBinCandidatesTests(unittest.TestCase):
    def test_windows_executable_suffix_variants(self):
        with (
            mock.patch.object(launcher.os, "name", "nt"),
            mock.patch.object(
                launcher.sysconfig, "get_config_var", return_value=".cmd"
            ),
        ):
            self.assertEqual(
                launcher.changepacks_exe_names(),
                ["changepacks.cmd", "changepacks.exe"],
            )

    def test_scripts_directory_ending_in_executable_name_is_still_a_directory(self):
        scripts_dir = os.path.join("root", "scripts", "changepacks")
        expected_first_candidate = os.path.join(scripts_dir, "changepacks")

        with (
            mock.patch.object(
                launcher.sysconfig, "get_path", return_value=scripts_dir
            ),
            mock.patch.object(
                launcher, "user_scripts_path", return_value="user-scripts"
            ),
            mock.patch.object(
                launcher, "changepacks_exe_names", return_value=["changepacks"]
            ),
            mock.patch.object(
                launcher.resources,
                "files",
                return_value=_ResourceDirectory(
                    {"changepacks": expected_first_candidate}
                ),
            ),
        ):
            candidates = launcher.changepacks_bin_candidates()

        self.assertEqual(candidates[0], expected_first_candidate)
        self.assertEqual(candidates.count(expected_first_candidate), 1)

    def test_direct_resource_candidates_follow_all_directory_candidates(self):
        scripts_dir = os.path.join("root", "scripts", "changepacks")
        user_scripts_dir = os.path.join("home", "Scripts")
        names = ["changepacks.cmd", "changepacks.exe"]
        resource_candidates = {
            "changepacks.cmd": os.path.join("wheel", "changepacks.cmd"),
            # Duplicate an earlier candidate to exercise stable deduplication.
            "changepacks.exe": os.path.join(scripts_dir, "changepacks.exe"),
        }
        package_dir = os.path.dirname(launcher.__file__)
        pkg_root = os.path.dirname(package_dir)
        search_dirs = [
            scripts_dir,
            user_scripts_dir,
            package_dir,
            pkg_root,
            os.path.join(pkg_root, "bin"),
            os.path.join(pkg_root, "Scripts"),
        ]
        expected = [
            os.path.join(directory, name)
            for directory in search_dirs
            for name in names
        ]
        expected.append(resource_candidates["changepacks.cmd"])

        with (
            mock.patch.object(
                launcher.sysconfig, "get_path", return_value=scripts_dir
            ),
            mock.patch.object(
                launcher, "user_scripts_path", return_value=user_scripts_dir
            ),
            mock.patch.object(
                launcher, "changepacks_exe_names", return_value=names
            ),
            mock.patch.object(
                launcher.resources,
                "files",
                return_value=_ResourceDirectory(resource_candidates),
            ),
        ):
            self.assertEqual(launcher.changepacks_bin_candidates(), expected)


class FindChangepacksBinTests(unittest.TestCase):
    def test_posix_skips_non_executable_file_before_executable_file(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            non_executable = os.path.join(temp_dir, "non-executable")
            executable = os.path.join(temp_dir, "executable")
            open(non_executable, "w").close()
            open(executable, "w").close()

            with (
                mock.patch.object(launcher.os, "name", "posix"),
                mock.patch.object(
                    launcher,
                    "changepacks_bin_candidates",
                    return_value=[non_executable, executable],
                ),
                mock.patch.object(
                    launcher.os,
                    "access",
                    side_effect=lambda path, mode: path == executable
                    and mode == os.X_OK,
                ),
            ):
                self.assertEqual(launcher.find_changepacks_bin(), executable)

    def test_windows_selects_file_without_executable_access_check(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            candidate = os.path.join(temp_dir, "changepacks.exe")
            open(candidate, "w").close()

            with (
                mock.patch.object(launcher.os, "name", "nt"),
                mock.patch.object(
                    launcher,
                    "changepacks_bin_candidates",
                    return_value=[candidate],
                ),
                mock.patch.object(launcher.os, "access") as access,
            ):
                self.assertEqual(launcher.find_changepacks_bin(), candidate)

            access.assert_not_called()

    def test_posix_finds_executable_in_pip_overlay_bin(self):
        build_env = os.path.join("tmp", "pip-build-env-posix")
        overlay_scripts = os.path.join(build_env, "overlay", "bin")
        normal_scripts = os.path.join(build_env, "normal", "bin")
        executable = os.path.join(overlay_scripts, "changepacks")

        with (
            mock.patch.object(launcher.os, "name", "posix"),
            mock.patch.dict(
                launcher.os.environ,
                {"PATH": os.pathsep.join([overlay_scripts, normal_scripts])},
            ),
            mock.patch.object(
                launcher, "changepacks_bin_candidates", return_value=[]
            ),
            mock.patch.object(
                launcher, "changepacks_exe_names", return_value=["changepacks"]
            ),
            mock.patch.object(
                launcher.os.path,
                "isfile",
                side_effect=lambda path: path == executable,
            ),
            mock.patch.object(
                launcher.os,
                "access",
                side_effect=lambda path, mode: path == executable
                and mode == os.X_OK,
            ) as access,
        ):
            self.assertEqual(launcher.find_changepacks_bin(), executable)

        access.assert_called_once_with(executable, os.X_OK)

    def test_windows_finds_file_in_pip_overlay_scripts(self):
        build_env = os.path.join("tmp", "pip-build-env-windows")
        overlay_scripts = os.path.join(build_env, "overlay", "Scripts")
        normal_scripts = os.path.join(build_env, "normal", "Scripts")
        executable = os.path.join(overlay_scripts, "changepacks.exe")

        with (
            mock.patch.object(launcher.os, "name", "nt"),
            mock.patch.dict(
                launcher.os.environ,
                {"PATH": os.pathsep.join([overlay_scripts, normal_scripts])},
            ),
            mock.patch.object(
                launcher, "changepacks_bin_candidates", return_value=[]
            ),
            mock.patch.object(
                launcher,
                "changepacks_exe_names",
                return_value=["changepacks.exe"],
            ),
            mock.patch.object(
                launcher.os.path,
                "isfile",
                side_effect=lambda path: path == executable,
            ),
            mock.patch.object(launcher.os, "access") as access,
        ):
            self.assertEqual(launcher.find_changepacks_bin(), executable)

        access.assert_not_called()

    def test_rejects_overlay_and_normal_from_different_build_environments(self):
        env_name = "pip-build-env-shared-name"
        overlay_scripts = os.path.join("tmp", "one", env_name, "overlay", "bin")
        normal_scripts = os.path.join("tmp", "two", env_name, "normal", "bin")

        with (
            mock.patch.object(launcher.os, "name", "posix"),
            mock.patch.dict(
                launcher.os.environ,
                {"PATH": os.pathsep.join([overlay_scripts, normal_scripts])},
            ),
            mock.patch.object(
                launcher, "changepacks_bin_candidates", return_value=[]
            ),
            mock.patch.object(
                launcher, "changepacks_exe_names", return_value=["changepacks"]
            ),
            mock.patch.object(launcher.os.path, "isfile", return_value=True) as isfile,
            mock.patch.object(launcher.os, "access", return_value=True),
        ):
            with self.assertRaises(FileNotFoundError):
                launcher.find_changepacks_bin()

        isfile.assert_not_called()

    def test_rejects_reversed_or_missing_build_environment_prefix(self):
        for env_name in ("demo-pip-build-env-", "pip-build-env"):
            with self.subTest(env_name=env_name):
                build_env = os.path.join("tmp", env_name)
                overlay_scripts = os.path.join(build_env, "overlay", "bin")
                normal_scripts = os.path.join(build_env, "normal", "bin")

                with (
                    mock.patch.object(launcher.os, "name", "posix"),
                    mock.patch.dict(
                        launcher.os.environ,
                        {
                            "PATH": os.pathsep.join(
                                [overlay_scripts, normal_scripts]
                            )
                        },
                    ),
                    mock.patch.object(
                        launcher, "changepacks_bin_candidates", return_value=[]
                    ),
                    mock.patch.object(
                        launcher,
                        "changepacks_exe_names",
                        return_value=["changepacks"],
                    ),
                    mock.patch.object(
                        launcher.os.path, "isfile", return_value=True
                    ) as isfile,
                    mock.patch.object(launcher.os, "access", return_value=True),
                ):
                    with self.assertRaises(FileNotFoundError):
                        launcher.find_changepacks_bin()

                isfile.assert_not_called()


if __name__ == "__main__":
    unittest.main()
