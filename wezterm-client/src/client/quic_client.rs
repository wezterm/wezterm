// QUIC client implementation
// Handles QUIC connections with certificate caching and SSH bootstrap
#![cfg(feature = "quic")]

use anyhow::{anyhow, bail, Context};
use codec::{GetTlsCredsResponse, Pdu};
use config::QuicDomainClient;
use mux::connui::ConnectionUI;
use quinn::crypto::rustls::QuicClientConfig as RustlsQuicClientConfig;
use quinn::rustls;
use smol::io::{AsyncRead, AsyncWrite};
use std::convert::TryFrom;
use std::io::Cursor;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use crate::client::AsyncReadAndWrite;

use super::Reconnectable;

/// Wraps a quinn bidirectional stream to implement AsyncReadAndWrite
///
/// Quinn provides futures-based streams which we adapt to the AsyncRead/AsyncWrite
/// trait interface. The key is that quinn's streams already implement
/// futures::io::AsyncRead/AsyncWrite, so we forward those implementations.
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
        // Quinn's RecvStream uses futures_io::AsyncRead but with quinn::ReadError.
        // We need to forward and convert the error type to std::io::Error.
        // Quinn's trait methods are available through the Deref implementation of Pin.
        match Pin::new(&mut self.recv).poll_read(cx, buf) {
            Poll::Ready(Ok(n)) => Poll::Ready(Ok(n)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("QUIC read error: {:?}", e),
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
        // Quinn's SendStream uses futures_io::AsyncWrite but with quinn::WriteError.
        // We need to forward and convert the error type to std::io::Error.
        // Quinn's trait methods are available through the Deref implementation of Pin.
        match Pin::new(&mut self.send).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => Poll::Ready(Ok(n)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("QUIC write error: {:?}", e),
            ))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        // QUIC doesn't require explicit flushing - data is sent when available
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        // Try to finish the send stream (non-blocking operation)
        match self.send.finish() {
            Ok(()) => Poll::Ready(Ok(())),
            Err(_e) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "QUIC send stream already closed or in error state",
            ))),
        }
    }
}

/// Configure QUIC transport parameters
fn configure_transport(
    config: Option<&config::QuicDomainClient>,
) -> Option<Arc<quinn::TransportConfig>> {
    if let Some(cfg) = config {
        let mut transport = quinn::TransportConfig::default();
        if let Ok(idle_timeout) = quinn::IdleTimeout::try_from(cfg.max_idle_timeout) {
            transport.max_idle_timeout(Some(idle_timeout));
        }
        // Default keep_alive_interval to half of max_idle_timeout if not explicitly set
        let keep_alive = cfg.keep_alive_interval.unwrap_or_else(|| {
            std::time::Duration::from_millis((cfg.max_idle_timeout.as_millis() / 2) as u64)
        });
        transport.keep_alive_interval(Some(keep_alive));
        Some(Arc::new(transport))
    } else {
        None
    }
}

/// Build rustls ClientConfig for QUIC, with optional client certificate
fn build_rustls_client_config(
    roots: rustls::RootCertStore,
    client_cert_pem: Option<String>,
) -> anyhow::Result<rustls::ClientConfig> {
    if let Some(cert_pem) = client_cert_pem {
        let mut cert_cursor = Cursor::new(cert_pem.as_bytes());

        // Extract certificate chain
        let certs: Vec<rustls::pki_types::CertificateDer> = rustls_pemfile::certs(&mut cert_cursor)
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to parse client certificate")?;

        if certs.is_empty() {
            anyhow::bail!("No certificates found in PEM");
        }

        // Extract private key by reading all PEM items and finding the key
        let mut key_cursor = Cursor::new(cert_pem.as_bytes());
        let mut private_key: Option<rustls::pki_types::PrivateKeyDer> = None;

        loop {
            match rustls_pemfile::read_one(&mut key_cursor) {
                Ok(Some(item)) => {
                    match item {
                        rustls_pemfile::Item::Pkcs8Key(key) => {
                            private_key = Some(rustls::pki_types::PrivateKeyDer::Pkcs8(key));
                            break;
                        }
                        rustls_pemfile::Item::Sec1Key(key) => {
                            private_key = Some(rustls::pki_types::PrivateKeyDer::Sec1(key));
                            break;
                        }
                        _ => {
                            // Skip other items
                            continue;
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    anyhow::bail!("Failed to read private key from PEM: {e}");
                }
            }
        }

        let private_key = private_key.ok_or_else(|| anyhow!("No private key found in PEM"))?;

        // Build config with client certificate
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(certs, private_key)
            .context("Failed to configure client certificate")
    } else {
        // Build config without client certificate
        Ok(rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth())
    }
}

/// Establish a QUIC connection to a remote mux server
pub async fn establish_quic_connection(
    remote_address: &str,
    client_cert_pem: Option<String>,
    ca_cert_pem: Option<String>,
    config: Option<&config::QuicDomainClient>,
) -> anyhow::Result<Box<dyn crate::client::AsyncReadAndWrite>> {
    use std::net::ToSocketAddrs;

    // Extract hostname for SNI, using expected_cn if provided
    let default_hostname = remote_address
        .split(':')
        .next()
        .ok_or_else(|| anyhow!("Missing hostname in remote_address"))?;
    let hostname = config
        .and_then(|c| c.expected_cn.as_deref())
        .unwrap_or(default_hostname);

    // Resolve hostname to socket address (handles both IPs and domain names)
    // This mirrors what TcpStream::connect does
    let socket_addr: std::net::SocketAddr = remote_address
        .to_socket_addrs()
        .context(format!("Failed to resolve address: {}", remote_address))?
        .next()
        .ok_or_else(|| anyhow!("No addresses found for {}", remote_address))?;

    // Check if connection migration is enabled
    let enable_migration = config.map(|c| c.enable_migration).unwrap_or(true);
    if enable_migration {
        log::debug!("Connection migration enabled - will handle network changes transparently");
    }

    // Create QUIC endpoint bound to any local address
    // Note: Connection migration is handled transparently by Quinn's protocol implementation.
    // If the network changes (e.g., WiFi to Ethernet), Quinn will automatically validate
    // the new path and continue the connection. The enable_migration flag ensures
    // we're using the default Quinn settings that support this.
    let mut endpoint =
        quinn::Endpoint::client("[::]:0".parse()?).context("Failed to create QUIC endpoint")?;

    // Build root certificate store
    let mut roots = rustls::RootCertStore::empty();

    // If CA certificate is provided, use it; otherwise use system roots
    if let Some(ca_pem) = ca_cert_pem {
        let mut cursor = Cursor::new(ca_pem.as_bytes());
        let certs: Vec<rustls::pki_types::CertificateDer> = rustls_pemfile::certs(&mut cursor)
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to parse CA certificate")?;

        for cert in certs {
            roots
                .add(cert)
                .context("Failed to add CA certificate to root store")?;
        }
    } else {
        // Fallback to system roots if no custom CA provided
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }

    // Load additional CA certificates from pem_root_certs
    if let Some(cfg) = config {
        let _ = config::pem_util::load_pem_root_certs(&cfg.pem_root_certs, |data| {
            let mut cursor = Cursor::new(data.as_slice());
            if let Ok(certs) = rustls_pemfile::certs(&mut cursor).collect::<Result<Vec<_>, _>>() {
                for cert in certs {
                    let _ = roots.add(cert);
                }
            }
            Ok(())
        });
    }

    // Build rustls client config (with or without client certificate)
    let client_crypto = build_rustls_client_config(roots, client_cert_pem)?;

    let quic_client_config = RustlsQuicClientConfig::try_from(client_crypto)?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_client_config));

    // Apply transport configuration from config
    if let Some(transport) = configure_transport(config) {
        client_config.transport_config(transport);
    }

    endpoint.set_default_client_config(client_config);

    // Connect to server
    log::info!(
        "QUIC: Initiating connection to {} (hostname: {})",
        socket_addr,
        hostname
    );
    let connecting = endpoint
        .connect(socket_addr, hostname)
        .context("Failed to create QUIC connection")?;

    let connection = connecting.await.context("QUIC handshake failed")?;

    log::info!("QUIC: Connection established, opening bidirectional stream");
    // Open a bidirectional stream for mux protocol
    let (send, recv) = connection
        .open_bi()
        .await
        .context("Failed to open QUIC stream")?;

    log::info!("QUIC: Stream opened successfully");

    let stream = Box::new(QuicStream::new(send, recv));
    Ok(stream)
}

fn try_quic_connect(
    remote_address: &str,
    creds: &codec::GetTlsCredsResponse,
    quic_client: &config::QuicDomainClient,
    source_description: &str,
) -> anyhow::Result<Box<dyn AsyncReadAndWrite>> {
    match smol::block_on(establish_quic_connection(
        remote_address,
        Some(creds.client_cert_pem.clone()),
        Some(creds.ca_cert_pem.clone()),
        Some(quic_client),
    )) {
        Ok(stream) => {
            log::info!("QUIC connection established from {}", source_description);
            Ok(stream)
        }
        Err(err) => {
            log::debug!("QUIC connect with {} failed: {}", source_description, err);
            Err(err)
        }
    }
}
impl Reconnectable {
    fn load_quic_creds_from_disk(
        &mut self,
        quic_client: &QuicDomainClient,
    ) -> anyhow::Result<Option<GetTlsCredsResponse>> {
        if !quic_client.persist_to_disk {
            return Ok(None);
        }

        let ca_path = self.tls_creds_ca_path()?;
        let cert_path = self.tls_creds_cert_path()?;

        if !ca_path.exists() || !cert_path.exists() {
            return Ok(None);
        }

        let ca_cert_pem = std::fs::read_to_string(&ca_path)?;
        let client_cert_pem = std::fs::read_to_string(&cert_path)?;

        Ok(Some(GetTlsCredsResponse {
            ca_cert_pem,
            client_cert_pem,
        }))
    }

    pub(super) fn save_quic_creds_to_disk(
        &self,
        quic_client: &QuicDomainClient,
        creds: &GetTlsCredsResponse,
    ) -> anyhow::Result<()> {
        if !quic_client.persist_to_disk {
            return Ok(());
        }

        let ca_path = self.tls_creds_ca_path()?;
        let cert_path = self.tls_creds_cert_path()?;

        std::fs::write(&ca_path, creds.ca_cert_pem.as_bytes())?;
        std::fs::write(&cert_path, creds.client_cert_pem.as_bytes())?;

        Ok(())
    }

    fn load_quic_creds_from_pem_files(
        &self,
        quic_client: &QuicDomainClient,
    ) -> anyhow::Result<Option<GetTlsCredsResponse>> {
        // Check if all required PEM files are specified
        let cert_path = match &quic_client.pem_cert {
            Some(path) => path,
            None => return Ok(None),
        };
        let key_path = match &quic_client.pem_private_key {
            Some(path) => path,
            None => return Ok(None),
        };
        let ca_path = match &quic_client.pem_ca {
            Some(path) => path,
            None => return Ok(None),
        };

        // Load certificate
        let mut client_cert_pem = std::fs::read_to_string(cert_path)
            .context(format!("reading client cert from {}", cert_path.display()))?;

        // Load and append private key
        let key_pem = std::fs::read_to_string(key_path)
            .context(format!("reading private key from {}", key_path.display()))?;
        client_cert_pem.push_str(&key_pem);

        // Load CA certificate
        let ca_cert_pem = std::fs::read_to_string(ca_path)
            .context(format!("reading CA cert from {}", ca_path.display()))?;

        Ok(Some(GetTlsCredsResponse {
            ca_cert_pem,
            client_cert_pem,
        }))
    }

    pub fn quic_connect(
        &mut self,
        quic_client: config::QuicDomainClient,
        _initial: bool,
        ui: &mut ConnectionUI,
    ) -> anyhow::Result<()> {
        let remote_address = &quic_client.remote_address;

        // Try in-memory cached credentials first
        if let Some(creds) = &self.tls_creds {
            log::debug!("Trying direct QUIC connection with in-memory cached credentials");
            match try_quic_connect(&remote_address, creds, &quic_client, "in-memory cache") {
                Ok(stream) => {
                    self.stream.replace(stream);
                    ui.output_str(&format!("QUIC Connected to {} (cached)!\n", remote_address));
                    return Ok(());
                }
                Err(_) => {
                    log::debug!(
                        "Failed to connect with in-memory credentials, trying alternatives"
                    );
                }
            }
        }

        // Try loading from explicit PEM files
        if let Ok(Some(creds)) = self.load_quic_creds_from_pem_files(&quic_client) {
            log::debug!("Loaded QUIC credentials from PEM files");
            // Validate that the certificate can be parsed
            if Self::is_certificate_valid(&creds.client_cert_pem) {
                log::debug!("PEM file credentials are valid, attempting QUIC connection");
                match try_quic_connect(&remote_address, &creds, &quic_client, "PEM files") {
                    Ok(stream) => {
                        self.stream.replace(stream);
                        self.tls_creds.replace(creds);
                        ui.output_str(&format!(
                            "QUIC Connected to {} (PEM files)!\n",
                            remote_address
                        ));
                        return Ok(());
                    }
                    Err(e) => {
                        log::debug!("Failed to connect with PEM file credentials: {:?}", e);
                    }
                }
            } else {
                log::debug!("PEM file credentials are invalid");
            }
        }

        // If bootstrap via SSH is configured, try to reuse persisted credentials before re-bootstrapping
        if let Some(Ok(ssh_params)) = quic_client.ssh_parameters() {
            // Try to load and validate credentials from disk before attempting SSH bootstrap
            if let Ok(Some(creds)) = self.load_quic_creds_from_disk(&quic_client) {
                log::debug!("Loaded QUIC credentials from disk, validating...");

                // Validate that the certificate can be parsed
                if Self::is_certificate_valid(&creds.client_cert_pem) {
                    log::debug!("Disk credentials are valid, attempting QUIC connection");
                    match try_quic_connect(&remote_address, &creds, &quic_client, "disk cache") {
                        Ok(stream) => {
                            self.stream.replace(stream);
                            self.tls_creds.replace(creds);
                            ui.output_str(&format!(
                                "QUIC Connected to {} (disk cache)!\n",
                                remote_address
                            ));
                            return Ok(());
                        }
                        Err(e) => {
                            log::debug!("Failed to connect with disk credentials: {:?}", e);
                            // Fall through to SSH bootstrap
                        }
                    }
                } else {
                    log::debug!("Disk credentials are expired or invalid");
                }
            }

            // SSH bootstrap for certificate exchange
            ui.output_str("Bootstrapping QUIC credentials via SSH...\n");

            let sess = crate::ssh_bootstrap::establish_ssh_session(&ssh_params, ui)?;

            // Execute tlscreds command to get certificates
            let cmd = format!(
                "{} cli tlscreds",
                Self::wezterm_bin_path(&quic_client.remote_wezterm_path)
            );
            ui.output_str(&format!("Running: {}\n", cmd));

            let creds = ui.run_and_log_error(|| {
                crate::ssh_bootstrap::execute_remote_command_for_pdu(&sess, &cmd, |pdu| match pdu {
                    Pdu::GetTlsCredsResponse(creds) => {
                        log::info!("got QUIC TLS creds");
                        Ok(creds)
                    }
                    _ => bail!("unexpected response to tlscreds"),
                })
            })?;

            // Save to disk if configured
            self.save_quic_creds_to_disk(&quic_client, &creds)?;

            // Now connect with the obtained credentials
            log::info!(
                "SSH bootstrap complete, now establishing QUIC connection to {}",
                remote_address
            );
            let stream = try_quic_connect(&remote_address, &creds, &quic_client, "SSH bootstrap")?;

            // Store stream and credentials in memory
            self.stream.replace(stream);
            self.tls_creds.replace(creds);
            ui.output_str(&format!("QUIC Connected to {}!\n", remote_address));
            Ok(())
        } else {
            bail!("No SSH bootstrap configured and no usable QUIC credentials found");
        }
    }
}
