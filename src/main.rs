//! Auto Clicker - Mouse & Key Presser - Click or press keys automatically at any speed, at one spot or a sequence of them.
//!
//!   auto_clicker.exe                 open the app
//!   auto_clicker.exe --auto-update   internal: background update check/download

#![windows_subsystem = "windows"]

mod app;
mod feedback;
mod control;
mod engine;
mod keys;
mod license;
mod profiles;
mod settings;
mod shots;
mod theme;
mod update;
mod winsys;

fn main() {
    // First thing, always: Velopack may need to run an install/uninstall/
    // update hook and exit before any of our own argument handling runs.
    update::run_app_hooks();

    let args: Vec<String> = std::env::args().skip(1).collect();

    // The detached child spawned below; must not spawn another.
    if args.first().map(String::as_str) == Some("--auto-update") {
        update::run_background_update();
        return;
    }
    if !args.iter().any(|a| a == "--shots") {
        update::maybe_spawn_background_update();
    }

    if let Err(e) = app::run(args) {
        eprintln!("{e}");
    }
}
