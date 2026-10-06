//! `zeus-native-host`: started by the browser. **Never write to stdout** except native-messaging frames; all
//! diagnostics go to stderr (which the browser discards) and never contain frame bodies.

use std::process::ExitCode;

use zeus_native_host::allowlist::{self, AllowList, Allowed};
use zeus_native_host::argv::parse_launch;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let launch = match parse_launch(&args) {
        Ok(launch) => launch,
        Err(error) => {
            eprintln!("zeus-native-host: {error}");
            return ExitCode::from(2);
        }
    };
    let list_path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(allowlist::FILE_NAME)));
    let allowed = list_path
        .as_deref()
        .map(AllowList::load)
        .transpose()
        .ok()
        .flatten()
        .map(|l| l.check(&launch.extension_id));
    match allowed {
        Some(Ok(Allowed::Production)) => {}
        Some(Ok(Allowed::Development)) => {
            eprintln!("zeus-native-host: WARNING: serving a development extension id")
        }
        _ => {
            eprintln!("zeus-native-host: the launching extension is not allowed");
            return ExitCode::from(3);
        }
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("zeus-native-host: cannot start the runtime: {error}");
            return ExitCode::from(1);
        }
    };
    match runtime.block_on(zeus_native_host::relay::run(&launch)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zeus-native-host: {error}");
            ExitCode::from(1)
        }
    }
}
