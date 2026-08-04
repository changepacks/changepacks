use std::fs;
use std::path::Path;

pub(crate) fn create_publish_wrapper(root: &Path) {
    #[cfg(windows)]
    fs::write(
        root.join("gradlew.bat"),
        "@echo off\r\n\
         setlocal EnableDelayedExpansion\r\n\
         echo cwd=%CD%\r\n\
         set /a argc=0\r\n\
         :capture_arg\r\n\
         if \"%~1\"==\"\" goto captured_args\r\n\
         echo argv[!argc!]=%~1\r\n\
         set /a argc+=1\r\n\
         shift\r\n\
         goto capture_arg\r\n\
         :captured_args\r\n\
         echo argc=!argc!\r\n",
    )
    .expect("fixture setup: failed to write gradlew.bat");

    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let wrapper = root.join("gradlew");
        fs::write(
            &wrapper,
            "#!/bin/sh\n\
             printf 'cwd=%s\\n' \"$PWD\"\n\
             printf 'argc=%s\\n' \"$#\"\n\
             index=0\n\
             for arg in \"$@\"; do\n\
               printf 'argv[%s]=%s\\n' \"$index\" \"$arg\"\n\
               index=$((index + 1))\n\
             done\n",
        )
        .expect("fixture setup: failed to write gradlew");
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))
            .expect("fixture setup: failed to make gradlew executable");
    }
}

/// Assert the mock wrapper ran in `expected`.
///
/// Both sides are resolved through [`fs::canonicalize`] before comparing. The
/// wrapper reports the directory the OS handed its process — on Unix a shell
/// resets `$PWD` to `getcwd()` when the inherited value does not describe the
/// current directory, and `%CD%` on Windows is already resolved — while the
/// test holds the path it created. Those two differ whenever the temporary root
/// is reached through a symlink, which is the default on macOS: `TempDir` hands
/// out `/var/folders/…`, a link to `/private/var/folders/…`.
///
/// # Panics
///
/// Panics if the wrapper produced no `cwd=` line, if either directory no longer
/// exists, or if the two resolve to different directories.
pub(crate) fn assert_reported_cwd(stdout: &str, expected: &Path) {
    let reported = stdout
        .lines()
        .find_map(|line| line.strip_prefix("cwd="))
        .expect("wrapper stdout must carry a `cwd=` line");

    assert_eq!(
        fs::canonicalize(reported).expect("the wrapper's reported directory must exist"),
        fs::canonicalize(expected).expect("the expected wrapper directory must exist"),
        "wrapper ran in the wrong directory; stdout: {stdout}"
    );
}

pub(crate) fn captured_argv(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            line.split_once("]=")
                .map(|(_, argument)| argument.to_owned())
        })
        .collect()
}
