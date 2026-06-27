#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    // Handle a couple of no-GUI flags before launching the window. This gives
    // CI a headless way to confirm the release binary actually loads and runs
    // (dynamic linker resolves, no startup panic) without needing a display.
    // The v2.0.0 Linux build linked but was unusable; `--version` under xvfb
    // is the cheap smoke test that would have caught it.
    let mut args = std::env::args().skip(1);
    if let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!(
                    "{name} {version}\n{desc}\n\nUSAGE:\n    {name} [OPTIONS]\n\n\
                     With no options, launches the graphical save editor.\n\n\
                     OPTIONS:\n    -V, --version    Print version and exit\n    \
                     -h, --help       Print this help and exit",
                    name = env!("CARGO_PKG_NAME"),
                    version = env!("CARGO_PKG_VERSION"),
                    desc = env!("CARGO_PKG_DESCRIPTION"),
                );
                return Ok(());
            }
            _ => {
                // Unknown args are ignored; fall through to launching the GUI.
            }
        }
    }

    tise::run_gui()
}
