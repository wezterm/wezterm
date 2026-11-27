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

`ServerAliveInterval` is now supported by the `libssh` backend.  Setting it to
a non-zero value will cause wezterm to send an `IGNORE` packet on that interval.
`ServerAliveCountMax` is NOT supported by this backend.  This keepalive
mechanism will not actively track the number of keepalives or disconnect the
session; the packets are sent in a fire-and-forget manner as a least effort way
to keep some traffic flowing on the connection to persuade intervening network
hardware to keep the session alive.

### SSH agent forwarding on Windows

{{since('nightly')}}

With the `LibSsh` [SSH backend](config/lua/config/ssh_backend.md), wezterm
supports authentication and agent forwarding through Windows named pipes,
including Windows OpenSSH's agent and PuTTY Pageant's OpenSSH-compatible pipe.
Set `ForwardAgent yes` for the destination and select the pipe with
`IdentityAgent` in your SSH configuration:

```sshconfig
Host my-server
    ForwardAgent yes
    IdentityAgent \\.\pipe\openssh-ssh-agent
```

Alternatively, set `SSH_AUTH_SOCK` or
[`default_ssh_auth_sock`](config/lua/config/default_ssh_auth_sock.md) to the
pipe path. An explicit `IdentityAgent` takes precedence.

For Pageant, start it with `--openssh-config` to generate a configuration file
containing its pipe path, then include that file for the desired host:

```console
pageant --openssh-config C:\Users\your-name\.ssh\pageant.conf
```

```sshconfig
Host my-server
    ForwardAgent yes
    Include pageant.conf
```

See [Pageant's OpenSSH integration documentation](https://tartarus.org/~simon/putty-snapshots/htmldoc/Chapter9.html#pageant-cmdline-openssh-config).
This uses Pageant's named pipe interface; the older window-message interface is
not used. After connecting with `wezterm ssh my-server`, `ssh-add -l` on the
remote host should list the keys currently held by the selected local agent.
Agent forwarding is not supported by wezterm's `Ssh2` backend.

### CLI Overrides

`wezterm ssh` CLI allows overriding config settings via the command line.  This
example shows how to specify the private key to use when connecting to
`some-host`:

```bash
wezterm ssh -oIdentityFile=/secret/id_ed25519 some-host
```

