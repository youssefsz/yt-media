//! Compiled child process used by process-runner contract tests.

#![forbid(unsafe_code)]

use std::{
    env,
    error::Error,
    ffi::OsString,
    fs::OpenOptions,
    io::{self, Read, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("process fixture failed: {error}");
        std::process::exit(125);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os();
    let _program = arguments.next();
    let command = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or("missing fixture command")?;
    let remaining = arguments.collect::<Vec<_>>();
    match command.as_str() {
        "argv" => print_arguments(&remaining)?,
        "interleave" => interleave(&remaining)?,
        "large-output" => large_output(&remaining)?,
        "exit" => exit_with(&remaining)?,
        "sleep" => sleep_for(&remaining)?,
        "stdin" => copy_stdin()?,
        "invalid-utf8" => invalid_utf8()?,
        "tree" => spawn_tree(&remaining)?,
        "leaf" => heartbeat(&remaining)?,
        _ => return Err(format!("unknown fixture command `{command}`").into()),
    }
    Ok(())
}

fn print_arguments(arguments: &[OsString]) -> Result<(), Box<dyn Error>> {
    let values = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    serde_json::to_writer(io::stdout().lock(), &values)?;
    Ok(())
}

fn interleave(arguments: &[OsString]) -> Result<(), Box<dyn Error>> {
    let count = parse_u64(arguments.first(), "count")?;
    let delay_ms = parse_u64(arguments.get(1), "delay")?;
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    for index in 0..count {
        writeln!(stdout, "out-{index}")?;
        stdout.flush()?;
        thread::sleep(Duration::from_millis(delay_ms));
        writeln!(stderr, "err-{index}")?;
        stderr.flush()?;
        thread::sleep(Duration::from_millis(delay_ms));
    }
    Ok(())
}

fn large_output(arguments: &[OsString]) -> Result<(), Box<dyn Error>> {
    let bytes_per_line = parse_u64(arguments.first(), "bytes per line")?;
    let lines = parse_u64(arguments.get(1), "line count")?;
    let bytes_per_line = usize::try_from(bytes_per_line)?;
    let line = vec![b'x'; bytes_per_line];
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    for _ in 0..lines {
        stdout.write_all(&line)?;
        stdout.write_all(b"\n")?;
        stderr.write_all(&line)?;
        stderr.write_all(b"\n")?;
    }
    stdout.flush()?;
    stderr.flush()?;
    Ok(())
}

fn exit_with(arguments: &[OsString]) -> Result<(), Box<dyn Error>> {
    let code = parse_i32(arguments.first(), "exit code")?;
    std::process::exit(code);
}

fn sleep_for(arguments: &[OsString]) -> Result<(), Box<dyn Error>> {
    let milliseconds = parse_u64(arguments.first(), "sleep duration")?;
    thread::sleep(Duration::from_millis(milliseconds));
    Ok(())
}

fn copy_stdin() -> Result<(), Box<dyn Error>> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    io::stdout().write_all(&input)?;
    Ok(())
}

fn invalid_utf8() -> Result<(), Box<dyn Error>> {
    io::stdout().write_all(&[0xff, 0xfe, b'\n'])?;
    Ok(())
}

fn spawn_tree(arguments: &[OsString]) -> Result<(), Box<dyn Error>> {
    let heartbeat_path = arguments.first().ok_or("missing heartbeat path")?;
    let current_executable = env::current_exe()?;
    let child = Command::new(current_executable)
        .arg("leaf")
        .arg(heartbeat_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "parent={}", std::process::id())?;
    writeln!(stdout, "child={}", child.id())?;
    stdout.flush()?;
    loop {
        thread::sleep(Duration::from_mins(1));
    }
}

fn heartbeat(arguments: &[OsString]) -> Result<(), Box<dyn Error>> {
    let path = arguments.first().ok_or("missing heartbeat path")?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(path))?;
    loop {
        file.write_all(b".")?;
        file.flush()?;
        thread::sleep(Duration::from_millis(25));
    }
}

fn parse_u64(value: Option<&OsString>, name: &str) -> Result<u64, Box<dyn Error>> {
    let value = value
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("missing or non-Unicode {name}"))?;
    Ok(value.parse()?)
}

fn parse_i32(value: Option<&OsString>, name: &str) -> Result<i32, Box<dyn Error>> {
    let value = value
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("missing or non-Unicode {name}"))?;
    Ok(value.parse()?)
}
