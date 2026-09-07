//! Minimal stderr logging (Info level and above), shared by daemon and GUI,
//! avoiding an extra dependency.

use std::sync::OnceLock;

static LOGGER: SimpleLogger = SimpleLogger;
static INIT: OnceLock<()> = OnceLock::new();

struct SimpleLogger;

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            // Timestamped so logs from the two processes can be correlated
            // when diagnosing timing issues.
            eprintln!(
                "[{}] [{}] {}",
                chrono::Local::now().format("%H:%M:%S"),
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

/// Initialize logging; safe to call multiple times.
pub fn init() {
    INIT.get_or_init(|| {
        let _ = log::set_logger(&LOGGER);
        log::set_max_level(log::LevelFilter::Info);
    });
}
