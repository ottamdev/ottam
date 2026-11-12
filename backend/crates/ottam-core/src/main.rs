#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write;

fn main() {
    let logging_guard = match ottam_core::logging::init_from_environment() {
        Ok(guard) => Some(guard),
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "ottam-core: failed to initialize logging: {error}"
            );
            None
        }
    };
    let logging_active = logging_guard.is_some();
    ottam_core::logging::install_panic_hook();

    if let Err(error) = ottam_core::serve_stdio() {
        tracing::error!(error = %error, "ottam-core exiting after server error");
        // This is a protocol message, not a log. Flush it before exiting so even
        // pre-initialize storage failures reach the client without log scraping.
        let notification = ottam_protocol::ProtocolNotification::new(
            ottam_protocol::METHOD_CORE_EXIT_ERROR,
            &error,
        );
        if let Ok(mut line) = serde_json::to_vec(&notification) {
            line.push(b'\n');
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(&line);
            let _ = stdout.flush();
        }
        if !logging_active {
            let _ = writeln!(std::io::stderr(), "ottam-core: server error: {error}");
        }
        // `std::process::exit` skips destructors, and both writers are
        // `tracing_appender` non-blocking workers that only flush when their
        // `WorkerGuard` drops. Exiting with the guard alive races that worker
        // for the line above: it survives on an idle machine and is lost about
        // half the time on a loaded one, from the log file as well as stderr —
        // so a Core that dies at startup leaves "logging initialized" as the
        // last thing it ever said.
        drop(logging_guard);
        std::process::exit(1);
    }
}
