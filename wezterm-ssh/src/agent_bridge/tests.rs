use super::*;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

fn run_test(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap()
        .block_on(smol::future::race(test, async {
            smol::Timer::after(Duration::from_secs(20)).await;
            panic!("agent bridge test timed out");
        }));
}

fn pipe_name() -> PathBuf {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    PathBuf::from(format!(
        r"\\.\pipe\wezterm-agent-test-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

fn server(name: &Path) -> NamedPipeServer {
    ServerOptions::new()
        .in_buffer_size(1024)
        .out_buffer_size(1024)
        .create(name)
        .unwrap()
}

fn connect(path: &Path) -> Async<Socket> {
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
    socket.connect(&SockAddr::unix(path).unwrap()).unwrap();
    Async::new(socket).unwrap()
}

// Echo arbitrary bytes, optionally transformed to distinguish two agents.
// Small pipe buffers force the bridge to cope with partial writes/backpressure.
fn echo_agent(name: &Path, xor: u8) -> tokio::task::JoinHandle<()> {
    let mut pipe = server(name);
    let name = name.to_owned();
    tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            pipe.connect().await.unwrap();
            let next = server(&name);
            connections.spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = pipe.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    for byte in &mut buf[..n] {
                        *byte ^= xor;
                    }
                    if pipe.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
            while connections.try_join_next().is_some() {}
            pipe = next;
        }
    })
}

async fn exchange(stream: &Async<Socket>, data: &[u8], xor: u8) {
    let ((), received) = smol::future::zip(
        async {
            let mut writer = stream;
            // Deliberately split at non-message/non-buffer boundaries.
            for chunk in data.chunks(373) {
                writer.write_all(chunk).await.unwrap();
            }
        },
        async {
            let mut reader = stream;
            let mut received = vec![0; data.len()];
            reader.read_exact(&mut received).await.unwrap();
            received
        },
    )
    .await;
    let expected: Vec<_> = data.iter().map(|byte| byte ^ xor).collect();
    assert_eq!(received, expected);
}

async fn assert_closed(mut stream: &Async<Socket>) {
    match stream.read(&mut [0u8; 1]).await {
        Ok(0) => {}
        Err(err) if err.kind() == io::ErrorKind::ConnectionReset => {}
        result => panic!("expected disconnected socket, got {:?}", result),
    }
}

#[test]
fn recognizes_openssh_and_pageant_pipe_paths() {
    for path in [
        r"\\.\pipe\openssh-ssh-agent",
        r"\\.\pipe\pageant.user.random",
        "//./pipe/pageant.user.random",
        r"\\.\PIPE\agent",
    ] {
        assert!(is_named_pipe(path), "{}", path);
    }
    for path in ["none", "", "/tmp/agent.sock", "C:/agent.sock", "中文"] {
        assert!(!is_named_pipe(path), "{}", path);
    }
}

#[test]
fn forwards_fragments_and_large_streams_without_decoding() {
    run_test(async {
        let name = pipe_name();
        let _agent = echo_agent(&name, 0);
        let (path, _bridge) = create_agent_bridge(name).unwrap();
        let stream = connect(&path);

        // Two bytes cannot form an agent frame. They must still pass through
        // immediately: the relay must not wait for a header or whole request.
        exchange(&stream, &[0xff, 0xff], 0).await;
        // Larger than the old 256 KiB protocol limit and both socket buffers.
        let data: Vec<_> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
        exchange(&stream, &data, 0).await;
        exchange(&stream, &[0, 0, 0, 1, 11, 0, 0, 0, 1, 11], 0).await;
    });
}

#[test]
fn sessions_do_not_replace_or_remove_each_others_sockets() {
    run_test(async {
        let name_a = pipe_name();
        let name_b = pipe_name();
        let _agent_a = echo_agent(&name_a, 0x11);
        let _agent_b = echo_agent(&name_b, 0x22);
        let (path_a, bridge_a) = create_agent_bridge(name_a).unwrap();
        let (path_b, bridge_b) = create_agent_bridge(name_b.clone()).unwrap();
        assert_ne!(path_a, path_b);

        let a = connect(&path_a);
        let b = connect(&path_b);
        exchange(&a, b"first agent", 0x11).await;
        exchange(&b, b"second agent", 0x22).await;
        drop(bridge_a);
        assert!(!path_a.exists());
        assert_closed(&a).await;
        // Reproduce the old lifetime bug without waiting days: keep one
        // session alive while repeatedly starting and dropping others.
        for _ in 0..20 {
            let (path, bridge) = create_agent_bridge(name_b.clone()).unwrap();
            exchange(&connect(&path), b"short session", 0x22).await;
            drop(bridge);
            assert!(!path.exists());
            exchange(&connect(&path_b), b"new forwarded channel", 0x22).await;
            exchange(&b, b"existing channel", 0x22).await;
        }
        drop(bridge_b);
        assert!(!path_b.exists());
    });
}

#[test]
fn recovers_after_agent_disconnect_and_restart() {
    run_test(async {
        let name = pipe_name();
        let agent = echo_agent(&name, 1);
        let (path, _bridge) = create_agent_bridge(name.clone()).unwrap();
        let old = connect(&path);
        exchange(&old, b"before restart", 1).await;
        agent.abort();
        let _ = agent.await;
        assert_closed(&old).await;
        // A failed connection while the agent is absent must not stop the
        // listener. The next connection must open the replacement pipe.
        assert_closed(&connect(&path)).await;
        let _agent = echo_agent(&name, 2);
        exchange(&connect(&path), b"after restart", 2).await;
    });
}

#[test]
fn closes_idle_pipe_when_client_disconnects() {
    run_test(async {
        let name = pipe_name();
        let mut pipe = server(&name);
        let (path, _bridge) = create_agent_bridge(name).unwrap();
        let stream = connect(&path);
        pipe.connect().await.unwrap();
        drop(stream);
        let result = pipe.read(&mut [0u8; 1]).await;
        assert!(matches!(result, Ok(0)) || result.is_err());
    });
}

#[test]
fn shutdown_cancels_unresponsive_agent_and_removes_socket() {
    run_test(async {
        let name = pipe_name();
        let pipe = server(&name);
        let (path, bridge) = create_agent_bridge(name).unwrap();
        let stream = connect(&path);
        pipe.connect().await.unwrap();
        // Leave the agent connected without replying. Drop must cancel pending
        // I/O, join the worker, and remove the socket without any agent activity.
        let start = Instant::now();
        drop(bridge);
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
        assert_closed(&stream).await;
    });
}

#[test]
fn retries_busy_pipe() {
    run_test(async {
        let name = pipe_name();
        let occupied = server(&name);
        let occupant = ClientOptions::new().open(&name).unwrap();
        occupied.connect().await.unwrap();
        let (path, bridge) = create_agent_bridge(name.clone()).unwrap();
        let stream = connect(&path);
        smol::Timer::after(Duration::from_millis(50)).await;
        // Make a new instance available while the first remains occupied.
        let _agent = echo_agent(&name, 0);
        exchange(&stream, b"available again", 0).await;
        drop(occupant);
        drop(bridge);
    });
}
