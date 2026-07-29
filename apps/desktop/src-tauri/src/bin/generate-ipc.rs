//! Generates or validates checked-in desktop IPC types.

#![forbid(unsafe_code)]

fn main() {
    let check = std::env::args()
        .skip(1)
        .any(|argument| argument == "--check");
    let path = yt_media_desktop::ipc::generated_typescript_path();
    if let Err(error) = yt_media_desktop::ipc::write_typescript(&path, check) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
