/// SSH bootstrap utilities for credential exchange
/// Shared between TLS and QUIC transport implementations
use anyhow::{anyhow, Context};
use codec::Pdu;
use config::SshParameters;
use mux::connui::ConnectionUI;
use portable_pty::Child;
use std::io::Read;
use std::thread;

/// Establish an SSH session for credential bootstrap
pub fn establish_ssh_session(
    ssh_params: &SshParameters,
    ui: &mut ConnectionUI,
) -> anyhow::Result<wezterm_ssh::Session> {
    let mut ssh_config = wezterm_ssh::Config::new();
    ssh_config.add_default_config_files();

    let mut fields = ssh_params.host_and_port.split(':');
    let host = fields
        .next()
        .ok_or_else(|| anyhow!("no host component somehow"))?;
    let port = fields.next();

    let mut ssh_config = ssh_config.for_host(host);
    if let Some(username) = &ssh_params.username {
        ssh_config.insert("user".to_string(), username.to_string());
    }
    if let Some(port) = port {
        ssh_config.insert("port".to_string(), port.to_string());
    }

    mux::ssh::ssh_connect_with_ui(ssh_config, ui)
}

/// Execute a remote command over SSH and extract a typed PDU response
///
/// This handles the common pattern of:
/// 1. Execute remote command
/// 2. Wait for completion
/// 3. Spawn stderr reader thread
/// 4. Decode PDU from stdout
/// 5. Extract specific PDU variant
/// 6. Return typed result
///
/// Note: The command string and logging is the caller's responsibility.
pub fn execute_remote_command_for_pdu<T>(
    sess: &wezterm_ssh::Session,
    cmd: &str,
    extract_pdu: impl FnOnce(Pdu) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut exec = smol::block_on(sess.exec(cmd, None))
        .with_context(|| format!("executing `{cmd}` on remote host"))?;

    log::debug!("waiting for command to finish");
    let status = exec.child.wait()?;
    if !status.success() {
        anyhow::bail!("{cmd} failed");
    }

    drop(exec.stdin);

    let mut stderr = exec.stderr;
    let cmd = String::from(cmd);
    thread::spawn(move || {
        // stderr is ideally empty
        let mut err = String::new();
        let _ = stderr.read_to_string(&mut err);
        if !err.is_empty() {
            log::error!("remote: `{cmd}` stderr -> `{err}`");
        }
    });

    let pdu = Pdu::decode(exec.stdout)
        .context("reading command response")?
        .pdu;

    extract_pdu(pdu)
}
