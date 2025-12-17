fn main() -> std::process::ExitCode {
    match bing_wallpaper_daily_mac_multimonitor::run_from_env() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
