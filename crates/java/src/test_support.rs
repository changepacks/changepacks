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
    // SAFE-UNWRAP: fixture setup failures must fail the test immediately.
    .unwrap();

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
        // SAFE-UNWRAP: fixture setup failures must fail the test immediately.
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))
            // SAFE-UNWRAP: fixture setup failures must fail the test immediately.
            .unwrap();
    }
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
