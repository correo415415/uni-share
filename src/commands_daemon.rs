//! `daemon start|stop|status|run`.

use crate::*;
use anyhow::Result;
use console::style;
use uni_share::daemon;

const S: &str = "DAEMON";

pub async fn daemon(ctx: Ctx, a: DaemonArgs) -> Result<()> {
    match a.action {
        DaemonAction::Start(r) => {
            let mut args = Vec::new();
            if let Some(o) = &r.output {
                args.push("--output".into());
                args.push(o.display().to_string());
            }
            if let Some(p) = r.port {
                args.push("--port".into());
                args.push(p.to_string());
            }
            if let Some(p) = &r.pin {
                args.push("--pin".into());
                args.push(p.clone());
            }
            if r.force {
                args.push("--force".into());
            }
            let pid = daemon::start(&args)?;
            ui::ok(S, format!("daemon started (pid {pid}); log: {}", daemon::log_file().display()));
            ui::info(S, "incoming transfers are accepted automatically; use `uni-share history` or the GUI to review them");
            Ok(())
        }
        DaemonAction::Stop => match daemon::stop()? {
            Some(pid) => {
                ui::ok(S, format!("daemon stopped (pid {pid})"));
                Ok(())
            }
            None => {
                ui::warn(S, "daemon is not running");
                Ok(())
            }
        },
        DaemonAction::Status => {
            let st = daemon::status();
            if st.running {
                ui::ok(S, format!("running (pid {})", style(st.pid.unwrap_or(0)).bold()));
            } else {
                ui::info(S, "not running");
            }
            ui::info(S, format!("pid file: {}   log: {}", st.pid_file.display(), st.log_file.display()));
            Ok(())
        }
        DaemonAction::Run(mut r) => {
            daemon::register_self()?;
            r.auto_accept = true;
            r.once = false;
            let res = crate::commands_lan::receive(Ctx { quiet: true, ..ctx }, r).await;
            daemon::unregister_self();
            res
        }
    }
}
