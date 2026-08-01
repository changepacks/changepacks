use std::io::{self, Write};

use clap::ValueEnum;

/// CLI output format selection.
///
/// Controls whether commands print human-readable output or JSON for CI integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FormatOptions {
    /// JSON format for CI/CD pipelines
    #[value(name = "json")]
    Json,
    /// Human-readable colored terminal output
    #[value(name = "stdout")]
    Stdout,
}

impl FormatOptions {
    /// Returns the payload this format prints for `stdout_msg`.
    ///
    /// `Self::Stdout` forwards the human-readable message unchanged, while
    /// `Self::Json` always yields the empty JSON object, ignoring `stdout_msg`.
    fn message(self, stdout_msg: &str) -> &str {
        match self {
            Self::Stdout => stdout_msg,
            Self::Json => "{}",
        }
    }

    /// Writes the payload for `stdout_msg` to `writer`, followed by a newline.
    ///
    /// Split out of [`Self::print`] so the emitted bytes can be asserted
    /// against an in-memory buffer instead of the process stdout.
    ///
    /// # Errors
    /// Propagates any write error reported by `writer`.
    fn write_message<W: Write>(self, writer: &mut W, stdout_msg: &str) -> io::Result<()> {
        writeln!(writer, "{}", self.message(stdout_msg))
    }

    /// Writes the payload for `stdout_msg` to a locked stdout handle.
    ///
    /// # Errors
    /// Returns the underlying [`io::Error`] when the write fails — most notably
    /// `BrokenPipe` when the consumer of a pipe (`changepacks publish | head`)
    /// exits early. Reporting it lets the caller fail cleanly instead of
    /// panicking inside `println!`.
    pub fn print(self, stdout_msg: &str) -> io::Result<()> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        self.write_message(&mut handle, stdout_msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    #[test]
    fn test_format_options_value_enum_json() {
        let format = FormatOptions::from_str("json", true).unwrap();
        assert!(matches!(format, FormatOptions::Json));
    }

    #[test]
    fn test_format_options_value_enum_stdout() {
        let format = FormatOptions::from_str("stdout", true).unwrap();
        assert!(matches!(format, FormatOptions::Stdout));
    }

    #[test]
    fn test_format_options_debug() {
        assert_eq!(format!("{:?}", FormatOptions::Json), "Json");
        assert_eq!(format!("{:?}", FormatOptions::Stdout), "Stdout");
    }

    #[test]
    fn test_format_options_message_stdout_forwards_message() {
        assert_eq!(
            FormatOptions::Stdout.message("No projects to publish"),
            "No projects to publish"
        );
        assert_eq!(FormatOptions::Stdout.message(""), "");
    }

    #[test]
    fn test_format_options_message_json_is_empty_object() {
        assert_eq!(FormatOptions::Json.message("No projects to publish"), "{}");
        assert_eq!(FormatOptions::Json.message("No updates to apply"), "{}");
        assert_eq!(FormatOptions::Json.message(""), "{}");
    }

    #[test]
    fn test_format_options_write_message_stdout_appends_newline() {
        let mut buffer = Vec::new();
        FormatOptions::Stdout
            .write_message(&mut buffer, "No projects found")
            .unwrap();
        assert_eq!(buffer, b"No projects found\n");
    }

    #[test]
    fn test_format_options_write_message_json_is_empty_object() {
        let mut buffer = Vec::new();
        FormatOptions::Json
            .write_message(&mut buffer, "No projects found")
            .unwrap();
        assert_eq!(buffer, b"{}\n");
    }

    #[test]
    fn test_format_options_write_message_propagates_error() {
        struct FailingWriter;

        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        for format in [FormatOptions::Stdout, FormatOptions::Json] {
            let error = format
                .write_message(&mut FailingWriter, "No updates found")
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
    }

    #[test]
    fn test_format_options_print_writes_to_stdout() {
        // Exercises the real stdout path for both variants; libtest captures
        // the output. The emitted bytes are asserted by the
        // `write_message` tests above, which share the same code path.
        FormatOptions::Stdout.print("No projects found").unwrap();
        FormatOptions::Json.print("No projects found").unwrap();
    }

    #[test]
    fn test_format_options_eq() {
        assert_eq!(FormatOptions::Json, FormatOptions::Json);
        assert_eq!(FormatOptions::Stdout, FormatOptions::Stdout);
        assert_ne!(FormatOptions::Json, FormatOptions::Stdout);
    }
}
