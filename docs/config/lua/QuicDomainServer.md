# QuicDomainServer

The `QuicDomainServer` struct specifies information about how to configure
a [QUIC Domain](../../multiplexing.md#quic-domains) server.

It is a lua object with the following fields:

```lua
config.quic_servers = {
  {
    -- The host:port combination on which the server will listen
    -- for client connections
    bind_address = 'server.hostname:9001',

    -- Load server certificates from PEM files (optional).
    -- All three files must be provided together for PEM file loading to be used:
    -- - pem_cert: x509 PEM encoded certificate file
    -- - pem_private_key: x509 PEM encoded private key file
    -- - pem_ca: x509 PEM encoded CA chain file
    -- If not specified, the server will generate ephemeral certificates (1 year lifetime).
    -- pem_private_key = "/some/path/key.pem",
    -- pem_cert = "/some/path/cert.pem",
    -- pem_ca = "/some/path/ca.pem",

    -- A set of paths to load additional CA certificates for client verification.
    -- Each entry can be either the path to a directory or to a PEM encoded
    -- CA file. If an entry is a directory, then its contents will be
    -- loaded as CA certs and added to the trust store.
    -- pem_root_certs = { "/some/path/ca1.pem", "/some/path/ca2.pem" },
  },
}
```
