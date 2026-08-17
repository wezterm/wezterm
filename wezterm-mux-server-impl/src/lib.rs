use config::{ConfigHandle, SshMultiplexing};
use mux::domain::{Domain, LocalDomain};
use mux::ssh::RemoteSshDomain;
use mux::Mux;
use std::sync::Arc;
use wezterm_client::domain::{ClientDomain, ClientDomainConfig};

pub mod dispatch;
pub mod local;
pub mod pki;
pub mod sessionhandler;

fn client_domains(config: &config::ConfigHandle) -> Vec<ClientDomainConfig> {
    let mut domains = vec![];
    for unix_dom in &config.unix_domains {
        domains.push(ClientDomainConfig::Unix(unix_dom.clone()));
    }

    for ssh_dom in config.ssh_domains().into_iter() {
        if ssh_dom.multiplexing == SshMultiplexing::WezTerm {
            domains.push(ClientDomainConfig::Ssh(ssh_dom.clone()));
        }
    }

    for tls_client in &config.tls_clients {
        domains.push(ClientDomainConfig::Tls(tls_client.clone()));
    }
    domains
}

pub fn update_mux_domains(config: &ConfigHandle) -> anyhow::Result<()> {
    update_mux_domains_impl(config, false)
}

pub fn update_mux_domains_for_server(config: &ConfigHandle) -> anyhow::Result<()> {
    update_mux_domains_impl(config, true)
}

/// Registers the `exec_domains` and `serial_ports` from `config` that `mux`
/// doesn't already know about.
///
/// Split out from `update_mux_domains_impl` so both of its paths can share it:
/// with an explicit `wsl_domains` list this runs inside
/// `cancel_domain_discovery_and_replace`'s closure, and without one it runs
/// directly. Safe in the former because it is pure in-memory work --
/// `LocalDomain::new_exec_domain` and `new_serial_domain` only build the domain
/// object (`SerialTty::new` just records the port name and settings; the port
/// isn't opened until a pane is spawned into it), so nothing here does I/O or
/// blocks while the discovery lock is held.
fn register_exec_and_serial_domains(mux: &Arc<Mux>, config: &ConfigHandle) -> anyhow::Result<()> {
    for exec_dom in &config.exec_domains {
        if mux.get_domain_by_name(&exec_dom.name).is_some() {
            continue;
        }

        let domain: Arc<dyn Domain> = Arc::new(LocalDomain::new_exec_domain(exec_dom.clone())?);
        mux.add_domain(&domain);
    }

    for serial in &config.serial_ports {
        if mux.get_domain_by_name(&serial.name).is_some() {
            continue;
        }

        let domain: Arc<dyn Domain> = Arc::new(LocalDomain::new_serial_domain(serial.clone())?);
        mux.add_domain(&domain);
    }

    Ok(())
}

/// Applies the configured default domain, if it names one that is registered
/// by now. Deliberately a no-op when it isn't: on the auto-discovery path the
/// name may belong to a WSL domain that hasn't been found yet, and discovery
/// applies it via `pending_default` once it appears.
///
/// Only takes `default_domain`'s lock (through `Mux::set_default_domain`), so
/// this is safe to call with the discovery lock held: `domain_discovery` is the
/// outer lock relative to `default_domain` everywhere in `mux`, never the
/// reverse.
fn apply_default_domain(
    mux: &Arc<Mux>,
    config: &ConfigHandle,
    is_standalone_mux: bool,
) -> anyhow::Result<()> {
    let configured = if is_standalone_mux {
        &config.default_mux_server_domain
    } else {
        &config.default_domain
    };

    if let Some(name) = configured {
        if let Some(dom) = mux.get_domain_by_name(name) {
            if is_standalone_mux && dom.is::<ClientDomain>() {
                anyhow::bail!("default_mux_server_domain cannot be set to a client domain!");
            }
            mux.set_default_domain(&dom);
        }
    }

    Ok(())
}

fn update_mux_domains_impl(config: &ConfigHandle, is_standalone_mux: bool) -> anyhow::Result<()> {
    let mux = Mux::get();

    for client_config in client_domains(&config) {
        if mux.get_domain_by_name(client_config.name()).is_some() {
            continue;
        }

        let domain: Arc<dyn Domain> = Arc::new(ClientDomain::new(client_config));
        mux.add_domain(&domain);
    }

    for ssh_dom in config.ssh_domains().into_iter() {
        if ssh_dom.multiplexing != SshMultiplexing::None {
            continue;
        }

        if mux.get_domain_by_name(&ssh_dom.name).is_some() {
            continue;
        }

        let domain: Arc<dyn Domain> = Arc::new(RemoteSshDomain::with_ssh_domain(&ssh_dom)?);
        mux.add_domain(&domain);
    }

    if let Some(wsl_domains) = &config.wsl_domains {
        // Use cancel_domain_discovery_and_replace to close the TOCTOU race
        // where a new waiter arriving between cancel and notify would see NotStarted
        // and do its lookup before replacement domains are registered.
        //
        // If a previous reload left an auto-discovery (from when
        // wsl_domains wasn't configured) still running in the background,
        // it's now stale: this config no longer wants discovered domains
        // or a discovery-driven default. Cancel it, register the explicit
        // wsl_domains atomically, then publish -- so every waiter -- sync or
        // async -- sees either the old discovery state or the new explicit
        // domains, never an intermediate state.
        //
        // The barrier deliberately spans the *whole* synchronous reload, not
        // just the WSL part: the exec domains, the serial ports and the new
        // default are all applied while the discovery lock is still held.
        // Publishing after only the WSL domains were registered would leave a
        // woken waiter able to observe a half-applied configuration -- looking
        // up an exec/serial domain the new config does define and being told it
        // is invalid, or reading the previous default. Since `mux` has no
        // WSL-specific knowledge, a waiter parked on discovery may well be
        // waiting for a name that has nothing to do with WSL, which is exactly
        // how that becomes observable.
        mux.cancel_domain_discovery_and_replace(|| {
            // Explicitly configured: already in memory and cheap to use, so
            // apply synchronously exactly as before. No need to shell out to
            // `wsl.exe`, so no reason to defer.
            for wsl_dom in wsl_domains {
                if mux.get_domain_by_name(&wsl_dom.name).is_some() {
                    continue;
                }

                let domain: Arc<dyn Domain> = Arc::new(LocalDomain::new_wsl(wsl_dom.clone())?);
                mux.add_domain(&domain);
            }

            register_exec_and_serial_domains(&mux, config)?;
            apply_default_domain(&mux, config, is_standalone_mux)?;

            Ok(())
        })?;
    } else {
        // No explicit list, so there is no in-flight discovery to cancel and
        // nothing to publish atomically against -- these can just be applied
        // directly. Registration still has to happen *before*
        // `begin_domain_discovery` below, so that the `pending_default` it
        // computes reflects a genuine "not discovered yet" rather than merely
        // "not reached in this function yet".
        register_exec_and_serial_domains(&mux, config)?;
        // Not configured: discovering the default list shells out to
        // `wsl.exe -l -v`, which can block for many seconds (starting the
        // LxssManager service and/or booting the WSL2 utility VM) on
        // Windows. Do that on a background thread instead of blocking
        // mux/gui startup on it (see mux::Mux::begin_domain_discovery,
        // which has no WSL-specific knowledge itself -- constructing the
        // actual WSL `LocalDomain`s happens in the closure below, kept
        // here alongside the rest of this function's domain-registration
        // logic). This runs after every synchronously-known domain
        // (client/ssh/exec/serial) has already been registered above, so
        // `pending_default` below reflects a genuine "not found yet"
        // rather than "hasn't been reached in this function yet". If the
        // configured default domain isn't known at this point, it may be
        // a WSL domain that's about to be discovered, so let discovery
        // apply it once found rather than leaving it unset.
        let default_domain_name = if is_standalone_mux {
            config.default_mux_server_domain.clone()
        } else {
            config.default_domain.clone()
        };
        let pending_default =
            default_domain_name.filter(|name| mux.get_domain_by_name(name).is_none());
        let discover = || {
            config::WslDomain::try_default_domains().map(|domains| {
                domains
                    .into_iter()
                    .filter_map(|wsl_dom| match LocalDomain::new_wsl(wsl_dom.clone()) {
                        Ok(domain) => Some(Arc::new(domain) as Arc<dyn Domain>),
                        Err(err) => {
                            log::error!(
                                "Error constructing wsl domain {}: {:#}",
                                wsl_dom.name,
                                err
                            );
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
        };
        if is_standalone_mux {
            // Keep the same "default_mux_server_domain cannot be a client
            // domain" invariant enforced below for the synchronous case
            // from being silently bypassed if the named domain is instead
            // found asynchronously by discovery.
            mux.begin_domain_discovery_with_default_filter(discover, pending_default, |dom| {
                !dom.is::<ClientDomain>()
            });
        } else {
            mux.begin_domain_discovery(discover, pending_default);
        }

        // Applied after `begin_domain_discovery` rather than before it purely
        // to preserve the original ordering; the call above doesn't register
        // anything synchronously, so the outcome is the same either way. If
        // the configured name isn't registered by now this is a no-op and
        // discovery applies it via `pending_default` once it finds it.
        apply_default_domain(&mux, config, is_standalone_mux)?;
    }

    Ok(())
}

lazy_static::lazy_static! {
    pub static ref PKI: pki::Pki = pki::Pki::init().expect("failed to initialize PKI");
}
