//! `tracing` initialisation. Verbosity: `-v` → debug, `-vv` → trace,
//! default → warn for deps / info for uni_share. `RUST_LOG` overrides all.

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn init(verbosity: u8, quiet: bool) {
    let default = if quiet {
        "error".to_string()
    } else {
        match verbosity {
            0 => "warn,uni_share=info".to_string(),
            1 => "info,uni_share=debug".to_string(),
            _ => "debug,uni_share=trace".to_string(),
        }
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let layer = fmt::layer()
        .with_target(verbosity >= 2)
        .with_writer(std::io::stderr)
        .compact();
    // Ring buffer + rotating file behind the "Registro" screens (GUI/app/API). The buffer
    // keeps everything the filter lets through; the file lives in `<data_dir>/logs/`.
    let logs_dir = crate::config::data_dir().join("logs");
    crate::logbuf::set_file_dir(&logs_dir);
    // Ignore error if a global subscriber was already set (tests).
    let _ = tracing_subscriber::registry().with(filter).with(layer).with(crate::logbuf::layer()).try_init();
}
