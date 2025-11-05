use crate::config::validate_domain_name;
use crate::*;
use wezterm_dynamic::{FromDynamic, ToDynamic};

#[derive(Default, Debug, Clone, FromDynamic, ToDynamic)]
pub struct QuicDomainServer {
    /// The address:port combination on which the server will listen
    /// for client connections
    pub bind_address: String,

    /// the path to an x509 PEM encoded private key file
    pub pem_private_key: Option<PathBuf>,

    /// the path to an x509 PEM encoded certificate file
    pub pem_cert: Option<PathBuf>,

    /// the path to an x509 PEM encoded CA chain file
    pub pem_ca: Option<PathBuf>,

    /// A set of paths to load additional CA certificates.
    /// Each entry can be either the path to a directory
    /// or to a PEM encoded CA file.  If an entry is a directory,
    /// then its contents will be loaded as CA certs and added
    /// to the trust store.
    #[dynamic(default)]
    pub pem_root_certs: Vec<PathBuf>,
}

#[derive(Default, Debug, Clone, FromDynamic, ToDynamic)]
pub struct QuicDomainClient {
    /// The name of this specific domain.  Must be unique amongst
    /// all types of domain in the configuration file.
    #[dynamic(validate = "validate_domain_name")]
    pub name: String,

    /// If set, use ssh to connect, start the server, and obtain
    /// a certificate.
    /// The value is "user@host:port", just like "wezterm ssh" accepts.
    pub bootstrap_via_ssh: Option<String>,

    /// identifies the host:port pair of the remote server.
    pub remote_address: String,

    /// Whether to persist QUIC certificates to disk (default: false)
    /// By default, certificates are kept only in memory for improved security.
    #[dynamic(default)]
    pub persist_to_disk: bool,

    /// Enable connection migration (default: true)
    #[dynamic(default = "default_true")]
    pub enable_migration: bool,

    /// Maximum idle timeout for QUIC connections
    #[dynamic(default = "default_max_idle_timeout")]
    pub max_idle_timeout: Duration,

    /// Keep-alive interval for QUIC connections
    pub keep_alive_interval: Option<Duration>,

    /// The path to an x509 PEM encoded private key file
    pub pem_private_key: Option<PathBuf>,

    /// The path to an x509 PEM encoded certificate file
    pub pem_cert: Option<PathBuf>,

    /// The path to an x509 PEM encoded CA chain file
    pub pem_ca: Option<PathBuf>,

    /// A set of paths to load additional CA certificates.
    /// Each entry can be either the path to a directory or to a PEM encoded
    /// CA file.  If an entry is a directory, then its contents will be
    /// loaded as CA certs and added to the trust store.
    #[dynamic(default)]
    pub pem_root_certs: Vec<PathBuf>,

    /// The hostname string that we expect to match against in the
    /// certificate presented by the server.  This defaults to
    /// the hostname portion of the `remote_address` configuration and you
    /// should not normally need to override this value.
    pub expected_cn: Option<String>,

    /// If true, connect to this domain automatically at startup
    #[dynamic(default)]
    pub connect_automatically: bool,

    #[dynamic(default = "default_local_echo_threshold_ms")]
    pub local_echo_threshold_ms: Option<u64>,

    /// The path to the wezterm binary on the remote host
    pub remote_wezterm_path: Option<String>,

    /// Show time since last response when waiting for a response.
    /// It is recommended to use
    /// <https://wezterm.org/config/lua/pane/get_metadata.html#since_last_response_ms>
    /// instead.
    #[dynamic(default)]
    pub overlay_lag_indicator: bool,
}

impl QuicDomainClient {
    pub fn ssh_parameters(&self) -> Option<anyhow::Result<SshParameters>> {
        self.bootstrap_via_ssh
            .as_ref()
            .map(|user_at_host_and_port| user_at_host_and_port.parse())
    }
}

fn default_max_idle_timeout() -> Duration {
    Duration::from_secs(30)
}
