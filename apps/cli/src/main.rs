//! Command-line entry point for YT Media.

#![forbid(unsafe_code)]

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build();
    let exit_code = match runtime {
        Ok(runtime) => runtime.block_on(yt_media_cli::run_environment()),
        Err(error) => {
            eprintln!("error: could not initialize the async runtime: {error}");
            yt_media_cli::ExitCode::InternalFailure
        }
    };
    std::process::exit(exit_code.value());
}
