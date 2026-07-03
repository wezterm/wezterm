wezterm uses an embedded ssh library to provide an integrated SSH client.  The
client can be used to make ad-hoc SSH connections to remote hosts
by invoking the client like this:

```console
$ wezterm ssh wez@my.server
```

(checkout `wezterm ssh -h` for more options).

When invoked in this way, wezterm may prompt you for SSH authentication
and once a connection is established, open a new terminal window with
your requested command, or your shell if you didn't specify one.

Creating new tabs or panes will each create a new channel in your existing
session so you won't need to re-authenticate for additional tabs that you
create.

SSH sessions created in this way are non-persistent and all associated
tabs will die if your network connection is interrupted.

Take a look at [the multiplexing section](multiplexing.md) for an
alternative configuration that connects to a remote wezterm instance
and preserves your tabs.

The [ssh_backend](config/lua/config/ssh_backend.md) configuration can
be used to specify which ssh library is used.

{{since('20210404-112810-b63a949d')}}

wezterm is now able to parse `~/.ssh/config` and `/etc/ssh/ssh_config`
and respects the following options:

* `IdentityAgent`
* `IdentityFile`
* `Hostname`
* `User`
* `Port`
* `ProxyCommand`
* `Host` (including wildcard matching)
* `UserKnownHostsFile`
* `IdentitiesOnly`
* `BindAddress`

All other options are parsed but have no effect.  Notably, neither `Match` or
`Include` will do anything.

{{since('20210502-154244-3f7122cb:')}}

`Match` is now recognized but currently supports only single-phase (`final`,
`canonical` are not supported) configuration parsing for `Host` and
`LocalUser`.  `Exec` based matches are recognized but not supported.

{{since('20210814-124438-54e29167:')}}

`Include` is now supported.

{{since('nightly')}}

`ProxyUseFDpass` is now supported. (But not on Microsoft Windows).

`IdentitiesOnly` is now correctly supported in the ssh2 backend when using agent authentication.
When `IdentitiesOnly=yes` is set in your ssh config, wezterm will filter the
keys offered by the SSH agent to only those whose public key matches a
configured `IdentityFile` entry.  Previously, `IdentitiesOnly=yes` caused agent
authentication to be skipped entirely. This also solves using YubiKeys, GPG, etc.

wezterm follows OpenSSH's own fallback order when locating the public key
material for an `IdentityFile` entry: first the file is tried as a public key
(so pointing `IdentityFile` directly at a `.pub` file works); then the
`<path>.pub` sibling is checked; and finally the public key is extracted from
the unencrypted envelope of the OpenSSH private key format, which works even
for passphrase-protected keys without prompting for the passphrase. No manual
`ssh-keygen -y` step is required.

Quoting in `ssh_config` values now follows OpenSSH's `argv_split`
rules:

* Double and single quotes are both honoured and are stripped from the
  stored value. `IdentityFile "/home/me/.ssh/path with space/id_rsa"`
  and `IdentityFile '/home/me/.ssh/path with space/id_rsa'` both yield
  the literal path `/home/me/.ssh/path with space/id_rsa`.
* Backslash escapes `\\`, `\"`, `\'` work inside and outside quotes;
  `\ ` (backslash followed by a space) works only outside quotes. For
  example `IdentityFile /home/me/.ssh/path\ with\ space/id_rsa`
  resolves to the same path as the quoted form above.
* An unquoted `#` at the **start** of a token terminates the rest of
  the line as a comment, matching OpenSSH's `argv_split` upstream.
  A `#` that appears inside quotes, inside a `${VAR}` reference, or
  in the middle of an unquoted token is preserved literally — paths
  containing `#` in their middle do not need to be quoted.
* A line with unbalanced quotes is dropped entirely and a
  `log::error!` is emitted identifying the offending line. OpenSSH
  itself aborts the whole config load in this case with "bad
  configuration options"; wezterm keeps the rest of the config
  working (because `Config::add_config_string` and friends have no
  error-return path) but raises the log level so the user notices
  that a directive vanished. If an `IdentityFile` or similar
  directive silently seems to have no effect, the error log is the
  first place to check.

Multiple `IdentityFile` directives accumulate as an ordered list and
are tried in the order they were declared (file-global first, then
each matching `Host`/`Match` stanza in the sequence they appear in
the config). Paths containing whitespace round-trip cleanly through
this typed list all the way to the ssh2 and libssh backends, so they
no longer collide with the separator used by the legacy
space-concatenated representation.

Repeated `-o IdentityFile=...` overrides on the command line
(e.g. `wezterm ssh -o IdentityFile=~/.ssh/key_a -o
IdentityFile=~/.ssh/key_b user@host`) now stack into that same
ordered list instead of the second override clobbering the first.

`ServerAliveInterval` is now supported by the `libssh` backend.  Setting it to
a non-zero value will cause wezterm to send an `IGNORE` packet on that interval.
`ServerAliveCountMax` is NOT supported by this backend.  This keepalive
mechanism will not actively track the number of keepalives or disconnect the
session; the packets are sent in a fire-and-forget manner as a least effort way
to keep some traffic flowing on the connection to persuade intervening network
hardware to keep the session alive.

### CLI Overrides

`wezterm ssh` CLI allows overriding config settings via the command line.  This
example shows how to specify the private key to use when connecting to
`some-host`:

```bash
wezterm ssh -oIdentityFile=/secret/id_ed25519 some-host
```

