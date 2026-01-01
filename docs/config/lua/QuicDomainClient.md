# QuicDomainClient

The `QuicDomainClient` struct specifies information about how to connect
to a [QUIC Domain](../../multiplexing.md#quic-domains).

It is a lua object with the following fields:

```lua
config.quic_clients = {
  {
    -- The name of this specific domain.  Must be unique amongst
    -- all types of domain in the configuration file.
    name = 'server.name',

    -- If set, use ssh to connect, start the server, and obtain
    -- a certificate.
    -- The value is "user@host:port", just like "wezterm ssh" accepts.
    bootstrap_via_ssh = 'server.hostname',

    -- identifies the host:port pair of the remote server.
    remote_address = 'server.hostname:9001',

    -- Whether to persist QUIC certificates to disk (default: false)
    -- By default, certificates are kept only in memory for improved security.
    -- persist_to_disk = false,

    -- Enable connection migration (default: true)
    -- Allows the connection to survive IP/port changes during mobility events
    -- enable_migration = true,

    -- Maximum idle timeout for QUIC connections in seconds (default: 30)
    -- max_idle_timeout = 30,

    -- Keep-alive interval for QUIC connections to prevent idle timeouts
    -- keep_alive_interval = 15,

    -- Load certificates directly from PEM files (optional, alternative to bootstrap_via_ssh).
    -- All three files must be provided together for PEM file loading to be used:
    -- - pem_cert: x509 PEM encoded certificate file
    -- - pem_private_key: x509 PEM encoded private key file
    -- - pem_ca: x509 PEM encoded CA chain file
    -- If all three are present, they take precedence over SSH bootstrap.
    -- pem_private_key = "/some/path/key.pem",
    -- pem_cert = "/some/path/cert.pem",
    -- pem_ca = "/some/path/ca.pem",

    -- A set of paths to load additional CA certificates for certificate
    -- verification. Each entry can be either the path to a directory or to a
    -- PEM encoded CA file. If an entry is a directory, then its contents will
    -- be loaded as CA certs and added to the trust store.
    -- pem_root_certs = { "/some/path/ca1.pem", "/some/path/ca2.pem" },

    -- The hostname string that we expect to match against in the certificate
    -- presented by the server.  This defaults to the hostname portion of
    -- remote_address and you should not normally need to override this value.
    -- expected_cn = "custom.hostname",

    -- If true, connect to this domain automatically at startup
    -- connect_automatically = false,

    -- The round-trip latency threshold in milliseconds for enabling predictive
    -- local echo (default: 100). If the measured round-trip latency between
    -- the wezterm client and server exceeds this threshold, the client will
    -- attempt to predict the server's response to key events and echo the
    -- result locally without waiting, hence hiding latency to the user.
    -- This option only applies when `multiplexing = "WezTerm"`.
    -- local_echo_threshold_ms = 100,

    -- The path to the wezterm binary on the remote host
    -- remote_wezterm_path = "/home/myname/bin/wezterm",

    -- Show time since last response when waiting for a response.
    -- It is recommended to use
    -- <https://wezterm.org/config/lua/pane/get_metadata.html#since_last_response_ms>
    -- instead.
    -- overlay_lag_indicator = false,
  },
}
```
