//! Optional end-to-end test against a configured SSH host and Windows agent.
//! Set WEZTERM_SSH_AGENT_TEST_HOST to a host in ~/.ssh/config. The host must
//! already be trusted, accept a key loaded in the agent, and have ssh-add.
//! WEZTERM_SSH_AGENT_TEST_PIPE can select Pageant's OpenSSH-compatible pipe.
//! WEZTERM_SSH_AGENT_TEST_CONFIG can supply a separate SSH configuration file.
//! Run with cargo test -p wezterm-ssh --test windows_agent_forward -- --ignored.

#![cfg(all(windows, feature = "libssh-rs"))]

use portable_pty::Child;
use std::io::Read;
use std::time::Duration;
use wezterm_ssh::{Config, ConfigMap, Session, SessionEvent};

async fn connect(config: &ConfigMap) -> Session {
    let (session, events) = Session::connect(config.clone()).unwrap();
    while let Ok(event) = events.recv().await {
        match event {
            SessionEvent::Banner(_) => {}
            SessionEvent::Authenticated => return session,
            event => panic!(
                "expected agent authentication to a trusted host: {:?}",
                event
            ),
        }
    }
    panic!("SSH session ended before authentication");
}

async fn list_keys(session: &Session) -> String {
    let mut exec = session.exec("ssh-add -L", None).await.unwrap();
    smol::unblock(move || {
        let mut stdout = String::new();
        let mut stderr = String::new();
        exec.stdout.read_to_string(&mut stdout).unwrap();
        exec.stderr.read_to_string(&mut stderr).unwrap();
        let status = exec.child.wait().unwrap();
        assert!(status.success(), "ssh-add failed: {}{}", stdout, stderr);
        assert!(!stdout.trim().is_empty());
        stdout
    })
    .await
}

#[test]
#[ignore = "requires a trusted SSH host and a running Windows agent with an authorized key"]
fn forwarding_survives_other_sessions_closing() {
    let host = std::env::var("WEZTERM_SSH_AGENT_TEST_HOST")
        .expect("set WEZTERM_SSH_AGENT_TEST_HOST to a host in ~/.ssh/config");
    let mut config = Config::new();
    if let Ok(path) = std::env::var("WEZTERM_SSH_AGENT_TEST_CONFIG") {
        config.add_config_file(path);
    } else {
        config.add_default_config_files();
    }
    let mut config = config.for_host(&host);
    config.insert("wezterm_ssh_backend".to_string(), "libssh".to_string());
    if let Ok(pipe) = std::env::var("WEZTERM_SSH_AGENT_TEST_PIPE") {
        config.insert("identityagent".to_string(), pipe);
    } else {
        config
            .entry("identityagent".to_string())
            .or_insert_with(|| r"\\.\pipe\openssh-ssh-agent".to_string());
    }
    config.insert("forwardagent".to_string(), "yes".to_string());
    config.insert("identitiesonly".to_string(), "no".to_string());
    // Do not use the host's configured private key file in place of the agent.
    config.insert(
        "identityfile".to_string(),
        "/nonexistent-wezterm-agent-test-key".to_string(),
    );

    smol::block_on(smol::future::race(
        async {
            let persistent = connect(&config).await;
            let expected = list_keys(&persistent).await;
            for _ in 0..5 {
                let temporary = connect(&config).await;
                assert_eq!(list_keys(&temporary).await, expected);
                drop(temporary);
                // Allow the old session's bridge cleanup to run before opening
                // another forwarding channel in the still-active session.
                smol::Timer::after(Duration::from_millis(150)).await;
                assert_eq!(list_keys(&persistent).await, expected);
            }
        },
        async {
            smol::Timer::after(Duration::from_secs(60)).await;
            panic!("Windows agent forwarding integration test timed out");
        },
    ));
}
