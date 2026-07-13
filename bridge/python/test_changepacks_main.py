import os
import unittest
from unittest import mock

from changepacks import __main__ as launcher


class _ResourceDirectory:
    def __init__(self, paths):
        self.paths = paths

    def joinpath(self, name):
        return self.paths[name]


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


if __name__ == "__main__":
    unittest.main()
