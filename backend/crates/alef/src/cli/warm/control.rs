// SPDX-License-Identifier: MIT OR Apache-2.0
//! `alef warm status|start|stop|enable|disable` (docs/stages/m11-warm-start.md,
//! «Архитектура реализации», «Управление»): status over the admin channel,
//! a detached start of this executable in the broker role, the admin
//! shutdown, and the autostart record on/off. Short English answers, like
//! the neighbouring CLI commands.

use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use alef_broker::autostart::{native, Entry, Status as AutostartStatus};
use alef_broker::ipc::token::load_or_create_token;
use alef_broker::ipc::{Client, ConnectError};
use alef_broker::protocol::message::{Hello, OsBytes, PROTOCOL_VERSION};
use alef_broker::protocol::warm::{
    decode_warm_admin_reply, encode_warm_admin_msg, AdminShutdown, AdminStatus, AdminStatusReply,
    WarmAdminMsg, WarmAdminReply,
};
use alef_launch::args::WarmCommand;

use super::endpoint::{endpoint, endpoint_present};
use crate::cli::failure::Failure;

/// How long `start`/`stop` wait for the broker to appear or disappear.
const WAIT: Duration = Duration::from_secs(5);
/// The pause between polls while waiting.
const POLL: Duration = Duration::from_millis(50);

pub(crate) fn warm_command(command: WarmCommand, json: bool) -> Result<u8, Failure> {
    match command {
        WarmCommand::Status => status(json),
        WarmCommand::Start => start(),
        WarmCommand::Stop => stop(),
        WarmCommand::Enable => enable(),
        WarmCommand::Disable => disable(),
    }
}

// -- status -------------------------------------------------------------

fn status(json: bool) -> Result<u8, Failure> {
    let broker = broker_status()?;
    let autostart = autostart_state()?;
    if json {
        println!("{}", status_json(broker.as_ref(), &autostart));
    } else {
        match &broker {
            Some(reply) => {
                println!(
                    "broker: running (pid {}, version {}, granted {}, running {})",
                    reply.pid, reply.version, reply.granted_total, reply.running
                );
                for slot in &reply.slots {
                    println!("  {:?}: {}", slot.kind, slot.state);
                }
            }
            None => println!("broker: not running"),
        }
        println!("autostart: {autostart}");
    }
    Ok(0)
}

/// One line of JSON with the same fields as the text status. Pure.
fn status_json(broker: Option<&AdminStatusReply>, autostart: &str) -> String {
    let broker = match broker {
        Some(reply) => serde_json::json!({
            "running": true,
            "pid": reply.pid,
            "version": reply.version,
            "granted_total": reply.granted_total,
            "running_apps": reply.running,
            "slots": reply.slots.iter().map(|slot| serde_json::json!({
                "kind": format!("{:?}", slot.kind).to_lowercase(),
                "state": slot.state,
            })).collect::<Vec<_>>(),
        }),
        None => serde_json::json!({ "running": false }),
    };
    serde_json::json!({ "broker": broker, "autostart": autostart }).to_string()
}

fn autostart_state() -> Result<String, Failure> {
    match native()
        .map_err(|e| Failure::Runtime(e.to_string()))?
        .status()
    {
        Ok(state) => Ok(String::from(match state {
            AutostartStatus::Enabled(_) => "enabled",
            AutostartStatus::Disabled => "disabled",
            AutostartStatus::Foreign => "foreign",
            AutostartStatus::Damaged => "damaged",
        })),
        Err(error) => Err(Failure::Runtime(error.to_string())),
    }
}

// -- start / stop -------------------------------------------------------

fn start() -> Result<u8, Failure> {
    if broker_status()?.is_some() {
        println!("already running");
        return Ok(0);
    }
    spawn_broker_detached()?;
    wait_for(|| broker_status().map_or(false, |s| s.is_some()))?;
    println!("started");
    Ok(0)
}

fn stop() -> Result<u8, Failure> {
    if broker_status()?.is_none() {
        println!("not running");
        return Ok(0);
    }
    admin_exchange(WarmAdminMsg::Shutdown(AdminShutdown))
        .map_err(|e| Failure::Runtime(format!("WARM_STOP_FAILED: {e}")))?;
    wait_for(|| broker_status().is_err())?;
    println!("stopped");
    Ok(0)
}

// -- enable / disable ---------------------------------------------------

fn enable() -> Result<u8, Failure> {
    let exe = current_exe()?;
    let entry = Entry::new(exe, vec![OsString::from("broker")]);
    native()
        .map_err(|e| Failure::Runtime(e.to_string()))?
        .enable(&entry)
        .map_err(|e| Failure::Runtime(e.to_string()))?;
    if broker_status()?.is_some() {
        println!("autostart enabled; broker already running");
        return Ok(0);
    }
    spawn_broker_detached()?;
    wait_for(|| broker_status().map_or(false, |s| s.is_some()))?;
    println!("autostart enabled; broker started");
    Ok(0)
}

fn disable() -> Result<u8, Failure> {
    // The broker keeps running: stopping it is the separate `alef warm stop`.
    native()
        .map_err(|e| Failure::Runtime(e.to_string()))?
        .disable()
        .map_err(|e| Failure::Runtime(e.to_string()))?;
    println!("autostart disabled");
    Ok(0)
}

// -- plumbing -----------------------------------------------------------

fn current_exe() -> Result<PathBuf, Failure> {
    std::env::current_exe().map_err(|e| Failure::Runtime(format!("current executable: {e}")))
}

fn broker_status() -> io::Result<Option<AdminStatusReply>> {
    match admin_exchange(WarmAdminMsg::Status(AdminStatus)) {
        Ok(WarmAdminReply::StatusReply(reply)) => Ok(Some(reply)),
        Ok(WarmAdminReply::Ack(_)) => Err(io::Error::new(
            ErrorKind::InvalidData,
            "unexpected answer to a status request",
        )),
        // No endpoint, no listener: the broker is simply not running.
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Whether a broker answers in this session right now.
pub(crate) fn broker_is_running() -> bool {
    broker_status().map_or(false, |status| status.is_some())
}

fn admin_exchange(msg: WarmAdminMsg) -> io::Result<WarmAdminReply> {
    let ep = endpoint().ok_or_else(|| {
        io::Error::new(
            ErrorKind::Unsupported,
            "warm start is unavailable on this system",
        )
    })?;
    if !endpoint_present(&ep) {
        return Err(io::Error::new(ErrorKind::NotFound, "broker is not running"));
    }
    let (token, _) = load_or_create_token(&ep)?;
    let mut client =
        Client::connect_with_hello(&ep, &token, hello()).map_err(|error| match error {
            ConnectError::Io(e) => e,
            other => io::Error::new(ErrorKind::ConnectionRefused, other.to_string()),
        })?;
    client.set_read_timeout(Some(WAIT))?;
    let body = encode_warm_admin_msg(&msg).map_err(io::Error::other)?;
    client.send_raw(&body)?;
    let reply = client.recv_raw()?;
    decode_warm_admin_reply(&reply)
        .map_err(|e| io::Error::new(ErrorKind::InvalidData, e.to_string()))
}

fn hello() -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        build: env!("CARGO_PKG_VERSION").to_owned(),
        alef_home: std::env::var_os("ALEF_HOME").map(|v| OsBytes::from_os_string(&v)),
        alef_app_home: std::env::var_os("ALEF_APP_HOME").map(|v| OsBytes::from_os_string(&v)),
    }
}

fn wait_for(mut ready: impl FnMut() -> bool) -> Result<(), Failure> {
    let deadline = Instant::now() + WAIT;
    loop {
        if ready() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Failure::Runtime(
                "WARM_TIMEOUT: the broker did not reach the expected state in time".to_owned(),
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// `current_exe broker` as a detached process: a new session on Unix, no
/// window and no console on Windows; stdio goes nowhere.
pub(crate) fn spawn_detached(exe: &Path, args: &[&str]) -> io::Result<()> {
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A new session: the broker survives the terminal that started it.
        unsafe {
            command.pre_exec(|| {
                let _ = libc::setsid();
                Ok(())
            });
        }
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    command.spawn().map(|_| ())
}

fn spawn_broker_detached() -> Result<(), Failure> {
    let exe = current_exe()?;
    spawn_detached(&exe, &["broker"])
        .map_err(|e| Failure::Runtime(format!("WARM_START_FAILED: the broker process: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(pid: u32) -> AdminStatusReply {
        AdminStatusReply {
            pid,
            version: "0.1.0".to_owned(),
            slots: vec![
                alef_broker::protocol::warm::SlotStateEntry {
                    kind: alef_broker::protocol::warm::SlotKind::Windowed,
                    state: "ready".to_owned(),
                },
                alef_broker::protocol::warm::SlotStateEntry {
                    kind: alef_broker::protocol::warm::SlotKind::Windowless,
                    state: "empty".to_owned(),
                },
            ],
            granted_total: 3,
            running: 2,
        }
    }

    #[test]
    fn the_json_status_is_one_line_with_the_same_fields() {
        let running = status_json(Some(&reply(12)), "enabled");
        assert!(!running.contains('\n'));
        assert!(running.contains("\"pid\":12"));
        assert!(running.contains("\"autostart\":\"enabled\""));
        assert!(running.contains("\"running\":true"));
        assert!(running.contains("\"windowed\""));
        let down = status_json(None, "disabled");
        assert!(!down.contains('\n'));
        assert!(down.contains("\"running\":false"));
        assert!(down.contains("\"autostart\":\"disabled\""));
    }
}
