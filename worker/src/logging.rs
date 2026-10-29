//! Send Rust log records to the Cloudflare Worker console.

use std::sync::Once;

use log::{Level, LevelFilter, Log, Metadata, Record};

static LOGGER: ConsoleLogger = ConsoleLogger;
static LOGGER_INIT: Once = Once::new();

struct ConsoleLogger;

/// Install the console logger once per Worker isolate.
pub fn init() {
    LOGGER_INIT.call_once(|| {
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(LevelFilter::Info);
        }
    });
}

impl Log for ConsoleLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let timestamp = String::from(js_sys::Date::new_0().to_iso_string());
        let line = format!(
            "{timestamp} {:<5} {}: {}",
            record.level(),
            record.target(),
            record.args()
        );
        match record.level() {
            Level::Error => worker::console_error!("{line}"),
            Level::Warn => worker::console_warn!("{line}"),
            Level::Info => worker::console_log!("{line}"),
            Level::Debug | Level::Trace => worker::console_debug!("{line}"),
        }
    }

    fn flush(&self) {}
}
