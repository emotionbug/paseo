use std::time::Duration;

fn main() {
    #[cfg(windows)]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "__paseo_pty_child")
    {
        // The ConPTY ownership gate must run before Tokio starts worker threads.
        finish(paseo_daemon::terminals::windows::run_child());
    }
    if std::env::args_os()
        .nth(1)
        .is_none_or(|arg| arg != "__worker")
    {
        let log = serde_json::json!({
            "level":std::env::var("PASEO_LOG_LEVEL").or_else(|_|std::env::var("PASEO_LOG")).unwrap_or_else(|_|"warn".into()),
            "format":std::env::var("PASEO_LOG_FORMAT").unwrap_or_else(|_|"json".into())
        });
        if let Err(error) = paseo_daemon::logging::initialize(&log) {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("paseo")
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Cannot start native runtime: {error}");
            std::process::exit(1);
        }
    };
    let result = runtime.block_on(paseo_daemon::cli::run());
    runtime.shutdown_timeout(Duration::from_secs(5));
    finish(result);
}

fn finish(result: anyhow::Result<i32>) -> ! {
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }
}
