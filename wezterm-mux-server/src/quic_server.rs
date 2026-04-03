// QUIC server implementation
// Handles QUIC endpoint setup and connection acceptance
#![cfg(feature = "quic")]

use anyhow::{anyhow, Context};
use config::QuicDomainServer;
use promise::spawn::spawn_into_main_thread;
use quinn::rustls;
use smol::io::{AsyncRead, AsyncWrite};
use std::convert::TryFrom;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

/// Wraps quinn streams to implement AsyncRead/AsyncWrite
#[derive(Debug)]
pub struct QuicStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

impl QuicStream {
    pub fn new(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self { send, recv }
    }
}

impl AsyncRead for QuicStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        // Use futures_lite::io::AsyncRead trait (imported at top)
        match Pin::new(&mut self.recv).poll_read(cx, buf) {
            Poll::Ready(Ok(n)) => Poll::Ready(Ok(n)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("QUIC read error: {e:?}"),
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for QuicStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // Use futures_lite::io::AsyncWrite trait (imported at top)
        match Pin::new(&mut self.send).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => Poll::Ready(Ok(n)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("QUIC write error: {e:?}"),
            ))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.send.finish() {
            Ok(()) => Poll::Ready(Ok(())),
            Err(_e) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "QUIC send stream already closed or in error state",
            ))),
        }
    }
}

/// Spawn a QUIC listener for the given configuration
pub fn spawn_quic_listener(quic_server: &QuicDomainServer) -> anyhow::Result<()> {
    // Parse bind address
    let listen_addr: std::net::SocketAddr = quic_server
        .bind_address
        .parse()
        .context("Invalid bind address for QUIC server")?;

    log::info!("QUIC server configured to listen on {}", listen_addr);

    // Clone config for the thread
    let quic_server_config = quic_server.clone();

    std::thread::spawn(move || {
        if let Err(e) = run_quic_listener(&quic_server_config) {
            log::error!("QUIC listener error: {e}");
        }
    });

    Ok(())
}

fn run_quic_listener(quic_server: &QuicDomainServer) -> anyhow::Result<()> {
    let listen_addr: std::net::SocketAddr = quic_server.bind_address.parse()?;

    // Determine certificate sources: explicit PEM files or PKI
    let (cert_data, ca_data) = if quic_server.pem_cert.is_some()
        && quic_server.pem_private_key.is_some()
        && quic_server.pem_ca.is_some()
    {
        // Load from explicit PEM files
        log::info!("Loading QUIC server certificates from PEM files");
        let cert_path = quic_server.pem_cert.as_ref().unwrap();
        let key_path = quic_server.pem_private_key.as_ref().unwrap();
        let ca_path = quic_server.pem_ca.as_ref().unwrap();

        let mut cert_data = std::fs::read(cert_path)
            .context(format!("reading server cert from {}", cert_path.display()))?;
        let key_data = std::fs::read(key_path)
            .context(format!("reading server key from {}", key_path.display()))?;
        cert_data.push(b'\n');
        cert_data.extend_from_slice(&key_data);

        let ca_data = std::fs::read(ca_path)
            .context(format!("reading CA cert from {}", ca_path.display()))?;

        (cert_data, ca_data)
    } else {
        // Fall back to PKI-generated ephemeral certificates
        log::info!("Loading QUIC server certificates from ephemeral PKI");
        let pki = &*wezterm_mux_server_impl::PKI;
        let cert_path = pki.server_pem();
        let ca_path = pki.ca_pem();

        let cert_data = std::fs::read(&cert_path)
            .context(format!("reading server cert from {}", cert_path.display()))?;
        let ca_data = std::fs::read(&ca_path)
            .context(format!("reading CA cert from {}", ca_path.display()))?;

        (cert_data, ca_data)
    };

    // Parse PEM into rustls format
    let cert_chain: Vec<rustls::pki_types::CertificateDer> =
        rustls_pemfile::certs(&mut std::io::Cursor::new(&cert_data))
            .collect::<Result<Vec<_>, _>>()
            .context("parsing server certificate")?;

    let mut key_reader = std::io::Cursor::new(&cert_data);
    let all_cert = rustls_pemfile::read_all(&mut key_reader);

    let mut server_key = None;

    for key_bytes in all_cert {
        let key_bytes = key_bytes.context("reading server key")?;

        let key = match key_bytes {
            rustls_pemfile::Item::Pkcs1Key(key) => rustls::pki_types::PrivateKeyDer::Pkcs1(key),
            rustls_pemfile::Item::Pkcs8Key(key) => rustls::pki_types::PrivateKeyDer::Pkcs8(key),
            rustls_pemfile::Item::Sec1Key(key) => rustls::pki_types::PrivateKeyDer::Sec1(key),
            bad => {
                log::debug!("Unwanted item {bad:?}");
                continue;
            }
        };
        log::debug!("Got key {key:?}");
        server_key = Some(key);
        break;
    }

    let server_key = server_key.ok_or_else(|| anyhow!("Missing server key"))?;

    // Load CA certificate for client verification
    let ca_certs: Vec<rustls::pki_types::CertificateDer> =
        rustls_pemfile::certs(&mut std::io::Cursor::new(&ca_data))
            .collect::<Result<Vec<_>, _>>()
            .context("parsing CA certificate")?;

    let mut ca_store = rustls::RootCertStore::empty();
    for cert in ca_certs {
        ca_store.add(cert).context("adding CA cert to store")?;
    }

    // Load additional CA certificates from pem_root_certs
    let _ = config::pem_util::load_pem_root_certs(&quic_server.pem_root_certs, |data| {
        let certs: Vec<rustls::pki_types::CertificateDer> =
            rustls_pemfile::certs(&mut std::io::Cursor::new(&data))
                .collect::<Result<Vec<_>, _>>()
                .unwrap_or_default();
        for cert in certs {
            let _ = ca_store.add(cert);
        }
        Ok(())
    });

    // Build rustls ServerConfig with client cert verification
    let server_config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(
            rustls::server::WebPkiClientVerifier::builder(Arc::new(ca_store))
                .build()
                .context("building client verifier")?,
        )
        .with_single_cert(cert_chain, server_key)
        .context("building server config")?;

    // Create Quinn ServerConfig from rustls
    let quinn_config = quinn::crypto::rustls::QuicServerConfig::try_from(server_config)
        .context("converting to QUIC config")?;
    let quinn_config = quinn::ServerConfig::with_crypto(Arc::new(quinn_config));

    // Bind UDP socket
    let socket = std::net::UdpSocket::bind(listen_addr).context("binding UDP socket")?;
    socket
        .set_nonblocking(true)
        .context("setting socket to non-blocking")?;

    // Create Quinn endpoint with smol runtime
    let runtime: Arc<dyn quinn::Runtime> = Arc::new(quinn::SmolRuntime);

    let endpoint = quinn::Endpoint::new(Default::default(), Some(quinn_config), socket, runtime)
        .context("creating QUIC endpoint")?;

    log::info!("QUIC server listening on {listen_addr}");

    // Run the accept loop in smol context
    let listener = async move {
        loop {
            match endpoint.accept().await {
                Some(connecting) => {
                    let connection = match connecting.await {
                        Ok(conn) => conn,
                        Err(e) => {
                            log::error!("QUIC handshake failed: {e}");
                            continue;
                        }
                    };

                    let peer_addr = connection.remote_address();
                    log::debug!("QUIC connection from {peer_addr}");

                    // Spawn stream handler
                    match connection.accept_bi().await {
                        Ok((send, recv)) => {
                            let stream = QuicStream::new(send, recv);

                            // Dispatch to main thread for mux processing
                            spawn_into_main_thread(async move {
                                wezterm_mux_server_impl::dispatch::process_async(stream)
                                    .await
                                    .map_err(|err| {
                                        log::error!("QUIC process error: {err}");
                                        err
                                    })
                            })
                            .detach();
                        }
                        Err(e) => {
                            log::error!("Failed to accept QUIC stream from {peer_addr}: {e}");
                        }
                    }
                }
                None => {
                    log::info!("QUIC endpoint closed");
                    break;
                }
            }
        }
    };

    spawn_into_main_thread(listener).detach();

    Ok(())
}
