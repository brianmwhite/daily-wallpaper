fn main() -> std::process::ExitCode {
    match daily_wallpaper::run_from_env() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
