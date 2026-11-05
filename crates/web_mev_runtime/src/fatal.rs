//! Reporting failures on stderr. Stderr is used directly because journald and
//! Docker capture it even when the tracing subscriber never started.

use std::error::Error;

/// Prints a startup failure with every underlying cause on its own line.
/// `Display` alone often hides the decisive detail, such as a read-only file
/// system under a migration error.
pub fn report_fatal(app: &str, error: &dyn Error) {
    eprint!("{}", fatal_message(app, error));
}

fn fatal_message(app: &str, error: &dyn Error) -> String {
    use std::fmt::Write as _;
    let mut message = format!("{app} failed to start: {error}\n");
    let mut source = error.source();
    while let Some(cause) = source {
        let _ = writeln!(message, "  caused by: {cause}");
        source = cause.source();
    }
    message
}

/// Replaces the default panic hook with one clear stderr line, so a panic in a
/// spawned task doesn't vanish without a trace.
pub fn install_panic_hook(app: &'static str) {
    std::panic::set_hook(Box::new(move |panic| {
        let location = panic.location().map_or_else(
            || "unknown location".to_string(),
            |location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            },
        );
        let message = panic
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("opaque panic payload");
        eprintln!("{app} panicked at {location}: {message}");
    }));
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::fatal_message;

    #[derive(Debug)]
    struct Wrapped(Option<Box<Wrapped>>, &'static str);

    impl fmt::Display for Wrapped {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.1)
        }
    }

    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.0.as_deref().map(|inner| inner as _)
        }
    }

    #[test]
    fn prints_every_cause() {
        let error = Wrapped(
            Some(Box::new(Wrapped(
                Some(Box::new(Wrapped(None, "read-only file system"))),
                "migration failed",
            ))),
            "database",
        );
        assert_eq!(
            fatal_message("app", &error),
            "app failed to start: database\n  caused by: migration failed\n  caused by: read-only file system\n"
        );
    }
}
