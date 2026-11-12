//! Thin wrappers around the `adb` binary.

use std::io;
use std::process::{Child, Command, Stdio};

pub fn devices() -> io::Result<Vec<String>> {
    let out = Command::new("adb").arg("devices").stdin(Stdio::null()).output()?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .filter_map(|l| l.strip_suffix("\tdevice"))
        .map(str::to_owned)
        .collect())
}

fn run(serial: &str, args: &[&str]) -> io::Result<String> {
    let out = Command::new("adb")
        .args(["-s", serial])
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(io::Error::other(format!("adb {}: {}", args.join(" "), err.trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn shell(serial: &str, cmd: &str) -> io::Result<String> {
    run(serial, &["shell", cmd])
}

/// Runs a shell command and returns its raw stdout (binary-safe).
pub fn exec_out(serial: &str, cmd: &str) -> io::Result<Vec<u8>> {
    let out = Command::new("adb")
        .args(["-s", serial, "exec-out", cmd])
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!("adb exec-out {cmd}: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(out.stdout)
}

pub fn push(serial: &str, local: &str, remote: &str) -> io::Result<()> {
    run(serial, &["push", local, remote]).map(drop)
}

/// Forwards a free local TCP port to an abstract socket on the device.
pub fn forward(serial: &str, abstract_name: &str) -> io::Result<u16> {
    let out = run(serial, &["forward", "tcp:0", &format!("localabstract:{abstract_name}")])?;
    out.trim()
        .parse()
        .map_err(|_| io::Error::other(format!("unexpected `adb forward` output: {out:?}")))
}

pub fn remove_forward(serial: &str, port: u16) {
    let _ = run(serial, &["forward", "--remove", &format!("tcp:{port}")]);
}

/// Starts a long-running `adb shell` command, with its output discarded.
pub fn spawn_shell(serial: &str, args: &[String]) -> io::Result<Child> {
    let debug = std::env::var_os("DEX_DEBUG").is_some();
    // DEX_DEBUG shows the server's own log (info on stdout, errors on stderr).
    Command::new("adb")
        .args(["-s", serial, "shell"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(if debug { Stdio::inherit() } else { Stdio::null() })
        .stderr(if debug { Stdio::inherit() } else { Stdio::piped() })
        .spawn()
}
