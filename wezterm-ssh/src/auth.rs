use crate::config::IdentityFileEntry;
use crate::pubkey_from_private::derive_public_blob;
use crate::session::SessionEvent;
use anyhow::Context;
use smol::channel::{bounded, Sender};
use std::path::Path;

#[derive(Debug)]
pub struct AuthenticationPrompt {
    pub prompt: String,
    pub echo: bool,
}

/// Return `true` when an `IdentityFile` entry can be used to sign
/// with (i.e. it names a private key, not a public one).
///
/// `IdentityFile` legitimately accepts a `.pub` path — the
/// agent-key filter still reads its blob via
/// [`derive_public_blob`] for matching — but the ssh2 backend's
/// `userauth_pubkey_file` and libssh's `AddIdentity` both require
/// a private-key path. Callers in the sign-capable code paths
/// gate with this helper to skip `.pub` entries without probing
/// bogus `<path>.pub.pub` siblings or prompting for spurious
/// passphrases.
pub(crate) fn is_identity_file_signable(entry: &IdentityFileEntry) -> bool {
    !entry.path.ends_with(".pub")
}

/// Collect allowed public key blobs from a list of [`IdentityFileEntry`]
/// values. For each entry the public key blob is obtained via
/// [`derive_public_blob`], which follows OpenSSH's own fallback
/// order (file-as-pub → sibling `.pub` → envelope of the private
/// key), so hardware-backed and passphrase-protected keys no longer
/// require a pre-generated `.pub` file sitting next to them.
#[cfg(feature = "ssh2")]
fn collect_identity_blobs(identity_files: &[IdentityFileEntry]) -> Vec<Vec<u8>> {
    let mut blobs = Vec::new();
    for entry in identity_files {
        let path = Path::new(&entry.path);
        match derive_public_blob(path) {
            Ok(Some(blob)) => {
                log::trace!("allowing agent key with blob {}", hex::encode(&blob));
                blobs.push(blob);
            }
            Ok(None) => {
                log::debug!(
                    "IdentityFile {:?} has no readable public or private key material",
                    entry.path
                );
            }
            Err(err) => {
                log::warn!(
                    "failed to derive public key from IdentityFile {:?}: {:#}",
                    entry.path,
                    err
                );
            }
        }
    }
    blobs
}

/// Determine the set of allowed agent key blobs based on the ssh config.
/// Returns `None` when all agent keys are allowed (IdentitiesOnly is not
/// set or not "yes"), or `Some(blobs)` with the restricted set.
#[cfg(feature = "ssh2")]
fn allowed_agent_key_blobs(
    config: &crate::config::ConfigMap,
    identity_files: &[IdentityFileEntry],
) -> Option<Vec<Vec<u8>>> {
    if config
        .get("identitiesonly")
        .map(|s| s.trim().eq_ignore_ascii_case("yes"))
        .unwrap_or(false)
    {
        Some(collect_identity_blobs(identity_files))
    } else {
        None
    }
}

#[derive(Debug)]
pub struct AuthenticationEvent {
    pub username: String,
    pub instructions: String,
    pub prompts: Vec<AuthenticationPrompt>,
    pub(crate) reply: Sender<Vec<String>>,
}

impl AuthenticationEvent {
    pub async fn answer(self, answers: Vec<String>) -> anyhow::Result<()> {
        Ok(self.reply.send(answers).await?)
    }

    pub fn try_answer(self, answers: Vec<String>) -> anyhow::Result<()> {
        Ok(self.reply.try_send(answers)?)
    }
}

impl crate::sessioninner::SessionInner {
    #[cfg(feature = "ssh2")]
    fn agent_auth(&mut self, sess: &ssh2::Session, user: &str) -> anyhow::Result<bool> {
        let mut agent = sess.agent()?;
        if agent.connect().is_err() {
            // If the agent is not around, we can proceed with other methods
            log::trace!("ssh agent not available");
            return Ok(false);
        }

        agent.list_identities()?;
        let identities = agent.identities()?;
        log::trace!("ssh agent has {} identities", identities.len());

        let allowed = allowed_agent_key_blobs(&self.config, &self.identity_files);

        let identities: Vec<_> = if let Some(allowed) = &allowed {
            identities
                .into_iter()
                .filter(|id| allowed.iter().any(|b| b.as_slice() == id.blob()))
                .collect()
        } else {
            identities
        };

        for identity in &identities {
            log::trace!(
                "considering agent key with blob {}",
                hex::encode(identity.blob())
            );
            if agent.userauth(user, identity).is_ok() {
                log::trace!(
                    "agent auth ok for key with blob {}",
                    hex::encode(identity.blob())
                );
                return Ok(true);
            }
            log::trace!(
                "agent auth failed for key with blob {}",
                hex::encode(identity.blob())
            );
        }
        log::trace!("agent auth failed for all keys");

        Ok(false)
    }

    #[cfg(feature = "ssh2")]
    fn pubkey_auth(
        &mut self,
        sess: &ssh2::Session,
        user: &str,
        host: &str,
    ) -> anyhow::Result<bool> {
        use std::path::{Path, PathBuf};

        // Clone so we can iterate without holding a borrow on `self`
        // while `tx_event.try_send` below wants &mut self access.
        let identity_files = self.identity_files.clone();
        for entry in &identity_files {
            // Skip entries whose path points at a public key file
            // (see `is_identity_file_signable`). `userauth_pubkey_file`
            // would otherwise probe a bogus `<path>.pub.pub` sibling
            // and likely prompt for a spurious passphrase. The
            // agent-key blob matching in `agent_auth` already handles
            // the `.pub` case correctly via `derive_public_blob`.
            if !is_identity_file_signable(entry) {
                log::trace!(
                    "pubkey_auth: skipping public-only identity entry {}",
                    entry.path
                );
                continue;
            }

            let pubkey: PathBuf = format!("{}.pub", entry.path).into();
            let file = Path::new(&entry.path);

            if !file.exists() {
                continue;
            }

            let pubkey = if pubkey.exists() {
                Some(pubkey.as_ref())
            } else {
                None
            };

            // We try with no passphrase first, in case the key is unencrypted
            match sess.userauth_pubkey_file(user, pubkey, &file, None) {
                Ok(_) => {
                    log::info!("pubkey_file immediately ok for {}", file.display());
                    return Ok(true);
                }
                Err(_) => {
                    // Most likely cause of error is that we need a passphrase
                    // to decrypt the key, so let's prompt the user for one.
                    let (reply, answers) = bounded(1);
                    self.tx_event
                        .try_send(SessionEvent::Authenticate(AuthenticationEvent {
                            username: "".to_string(),
                            instructions: "".to_string(),
                            prompts: vec![AuthenticationPrompt {
                                prompt: format!(
                                    "Passphrase to decrypt {} for {}@{}:\n> ",
                                    file.display(),
                                    user,
                                    host
                                ),
                                echo: false,
                            }],
                            reply,
                        }))
                        .context("sending Authenticate request to user")?;

                    let answers = smol::block_on(answers.recv())
                        .context("waiting for authentication answers from user")?;

                    if answers.is_empty() {
                        anyhow::bail!("user cancelled authentication");
                    }

                    let passphrase = &answers[0];

                    match sess.userauth_pubkey_file(user, pubkey, &file, Some(passphrase)) {
                        Ok(_) => {
                            return Ok(true);
                        }
                        Err(err) => {
                            log::warn!("pubkey auth: {:#}", err);
                        }
                    }
                }
            }
        }
        Ok(false)
    }

    #[cfg(feature = "libssh-rs")]
    pub fn authenticate_libssh(&mut self, sess: &libssh_rs::Session) -> anyhow::Result<()> {
        use std::collections::HashMap;
        let tx = self.tx_event.clone();

        // Set the callback for pubkey auth
        sess.set_auth_callback(move |prompt, echo, _verify, identity| {
            let (reply, answers) = bounded(1);
            tx.try_send(SessionEvent::Authenticate(AuthenticationEvent {
                username: "".to_string(),
                instructions: "".to_string(),
                prompts: vec![AuthenticationPrompt {
                    prompt: match identity {
                        Some(ident) => format!("{} ({}): ", prompt, ident),
                        None => prompt.to_string(),
                    },
                    echo,
                }],
                reply,
            }))
            .unwrap();

            let mut answers = smol::block_on(answers.recv())
                .context("waiting for authentication answers from user")
                .unwrap();
            Ok(answers.remove(0))
        });

        use libssh_rs::{AuthMethods, AuthStatus};
        match sess.userauth_none(None)? {
            AuthStatus::Success => return Ok(()),
            _ => {}
        }

        loop {
            let auth_methods = sess.userauth_list(None)?;
            let mut status_by_method = HashMap::new();

            if auth_methods.contains(AuthMethods::PUBLIC_KEY) {
                match sess.userauth_public_key_auto(None, None)? {
                    AuthStatus::Success => return Ok(()),
                    AuthStatus::Partial => continue,
                    status => {
                        status_by_method.insert(AuthMethods::PUBLIC_KEY, status);
                    }
                }
            }

            if auth_methods.contains(AuthMethods::INTERACTIVE) {
                loop {
                    match sess.userauth_keyboard_interactive(None, None)? {
                        AuthStatus::Success => return Ok(()),
                        AuthStatus::Info => {
                            let info = sess.userauth_keyboard_interactive_info()?;

                            let (reply, answers) = bounded(1);
                            self.tx_event
                                .try_send(SessionEvent::Authenticate(AuthenticationEvent {
                                    username: sess.get_user_name()?,
                                    instructions: info.instruction,
                                    prompts: info
                                        .prompts
                                        .into_iter()
                                        .map(|p| AuthenticationPrompt {
                                            prompt: p.prompt,
                                            echo: p.echo,
                                        })
                                        .collect(),
                                    reply,
                                }))
                                .context("sending Authenticate request to user")?;

                            let answers = smol::block_on(answers.recv())
                                .context("waiting for authentication answers from user")?;

                            sess.userauth_keyboard_interactive_set_answers(&answers)?;

                            continue;
                        }
                        AuthStatus::Denied => {
                            break;
                        }
                        AuthStatus::Partial => continue,
                        status => {
                            anyhow::bail!("interactive auth status: {:?}", status);
                        }
                    }
                }
            }

            if auth_methods.contains(AuthMethods::PASSWORD) {
                let (reply, answers) = bounded(1);
                self.tx_event
                    .try_send(SessionEvent::Authenticate(AuthenticationEvent {
                        username: "".to_string(),
                        instructions: "".to_string(),
                        prompts: vec![AuthenticationPrompt {
                            prompt: "Password: ".to_string(),
                            echo: false,
                        }],
                        reply,
                    }))
                    .unwrap();

                let mut answers = smol::block_on(answers.recv())
                    .context("waiting for authentication answers from user")
                    .unwrap();
                let pw = answers.remove(0);

                match sess.userauth_password(None, Some(&pw))? {
                    AuthStatus::Success => return Ok(()),
                    AuthStatus::Partial => continue,
                    status => anyhow::bail!("password auth status: {:?}", status),
                }
            }

            anyhow::bail!(
                "unhandled auth case; methods={:?}, status={:?}",
                auth_methods,
                status_by_method
            );
        }
    }

    #[cfg(feature = "ssh2")]
    pub fn authenticate(
        &mut self,
        sess: &ssh2::Session,
        user: &str,
        host: &str,
    ) -> anyhow::Result<()> {
        use std::collections::HashSet;

        loop {
            if sess.authenticated() {
                return Ok(());
            }

            // Re-query the auth methods on each loop as a successful method
            // may unlock a new method on a subsequent iteration (eg: password
            // auth may then unlock 2fac)
            let methods: HashSet<&str> = sess.auth_methods(&user)?.split(',').collect();
            log::trace!("ssh auth methods: {:?}", methods);

            if !sess.authenticated() && methods.contains("publickey") {
                if self.agent_auth(sess, user)? {
                    continue;
                }

                if self.pubkey_auth(sess, user, host)? {
                    continue;
                }
            }

            if !sess.authenticated() && methods.contains("password") {
                let (reply, answers) = bounded(1);
                self.tx_event
                    .try_send(SessionEvent::Authenticate(AuthenticationEvent {
                        username: user.to_string(),
                        instructions: "".to_string(),
                        prompts: vec![AuthenticationPrompt {
                            prompt: format!("Password for {}@{}: ", user, host),
                            echo: false,
                        }],
                        reply,
                    }))
                    .context("sending Authenticate request to user")?;

                let answers = smol::block_on(answers.recv())
                    .context("waiting for authentication answers from user")?;

                if answers.is_empty() {
                    anyhow::bail!("user cancelled authentication");
                }

                if let Err(err) = sess.userauth_password(user, &answers[0]) {
                    log::error!("while attempting password auth: {}", err);
                }
            }

            if !sess.authenticated() && methods.contains("keyboard-interactive") {
                struct Helper<'a> {
                    tx_event: &'a Sender<SessionEvent>,
                }

                impl<'a> ssh2::KeyboardInteractivePrompt for Helper<'a> {
                    fn prompt<'b>(
                        &mut self,
                        username: &str,
                        instructions: &str,
                        prompts: &[ssh2::Prompt<'b>],
                    ) -> Vec<String> {
                        let (reply, answers) = bounded(1);
                        if let Err(err) = self.tx_event.try_send(SessionEvent::Authenticate(
                            AuthenticationEvent {
                                username: username.to_string(),
                                instructions: instructions.to_string(),
                                prompts: prompts
                                    .iter()
                                    .map(|p| AuthenticationPrompt {
                                        prompt: p.text.to_string(),
                                        echo: p.echo,
                                    })
                                    .collect(),
                                reply,
                            },
                        )) {
                            log::error!("sending Authenticate request to user: {:#}", err);
                            return vec![];
                        }

                        match smol::block_on(answers.recv()) {
                            Err(err) => {
                                log::error!(
                                    "waiting for authentication answers from user: {:#}",
                                    err
                                );
                                return vec![];
                            }
                            Ok(answers) => answers,
                        }
                    }
                }

                let mut helper = Helper {
                    tx_event: &self.tx_event,
                };

                if let Err(err) = sess.userauth_keyboard_interactive(user, &mut helper) {
                    log::error!("while attempting keyboard-interactive auth: {}", err);
                }
            }
        }
    }
}

#[cfg(all(test, feature = "ssh2"))]
mod tests {
    use super::*;
    use base64::Engine;

    /// Create a temporary directory with a unique name and return its path.
    /// The caller is responsible for removing it (see `cleanup`).
    fn tmpdir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wezterm_ssh_test_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn entries_from(paths: &[&str]) -> Vec<IdentityFileEntry> {
        paths
            .iter()
            .map(|p| IdentityFileEntry {
                path: (*p).to_string(),
            })
            .collect()
    }

    #[test]
    fn collect_blobs_from_pub_file() {
        let dir = tmpdir("blobs_pub");
        let pub_path = dir.join("id_test.pub");
        std::fs::write(&pub_path, "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAA test\n").unwrap();

        let priv_path = dir.join("id_test");
        let entries = entries_from(&[priv_path.to_str().unwrap()]);

        let blobs = collect_identity_blobs(&entries);
        assert_eq!(blobs.len(), 1);
        assert_eq!(
            blobs[0],
            base64::engine::general_purpose::STANDARD
                .decode("AAAAB3NzaC1yc2EAAAADAQABAAAA")
                .unwrap()
        );
        cleanup(&dir);
    }

    #[test]
    fn collect_blobs_from_pub_content_at_private_path() {
        // If the path itself contains a parseable public key (e.g. user
        // pointed IdentityFile directly at the .pub file)
        let dir = tmpdir("blobs_priv_path");
        let path = dir.join("id_test.pub");
        std::fs::write(&path, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIQ==\n").unwrap();

        let entries = entries_from(&[path.to_str().unwrap()]);
        let blobs = collect_identity_blobs(&entries);
        assert_eq!(blobs.len(), 1);
        cleanup(&dir);
    }

    #[test]
    fn collect_blobs_missing_files() {
        let entries = entries_from(&["/nonexistent/path/id_key"]);
        let blobs = collect_identity_blobs(&entries);
        assert!(blobs.is_empty());
    }

    #[test]
    fn collect_blobs_multiple_identity_files() {
        let dir = tmpdir("blobs_multi");

        let pub1 = dir.join("key1.pub");
        std::fs::write(&pub1, "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAA k1\n").unwrap();

        let pub2 = dir.join("key2.pub");
        std::fs::write(&pub2, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIQ== k2\n").unwrap();

        let key1 = dir.join("key1");
        let key2 = dir.join("key2");
        let entries = entries_from(&[key1.to_str().unwrap(), key2.to_str().unwrap()]);
        let blobs = collect_identity_blobs(&entries);
        assert_eq!(blobs.len(), 2);
        cleanup(&dir);
    }

    #[test]
    fn collect_blobs_preserves_space_in_path() {
        // Regression guard for the original motivation of the refactor:
        // a private key file living in a directory whose name contains
        // a space must be readable via the typed identity list. The
        // legacy space-concatenated ConfigMap value cannot represent
        // this case unambiguously.
        let dir = tmpdir("blobs_space dir");
        let pub_path = dir.join("id_test.pub");
        std::fs::write(
            &pub_path,
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIQ== spaced\n",
        )
        .unwrap();
        let priv_path = dir.join("id_test");
        let entries = entries_from(&[priv_path.to_str().unwrap()]);
        let blobs = collect_identity_blobs(&entries);
        assert_eq!(blobs.len(), 1);
        cleanup(&dir);
    }

    #[test]
    fn collect_blobs_accepts_identity_file_pointing_at_dotpub_directly() {
        // `IdentityFile` can legally point directly at a `.pub` file
        // (e.g. when the user only wants agent-auth for that key and
        // the private half lives elsewhere or on a hardware token).
        // The typed list must still yield the blob for the agent
        // filter, even though `pubkey_auth` will skip the entry as
        // unsignable (see `is_identity_file_signable`).
        let dir = tmpdir("blobs_direct_pub");
        let pub_path = dir.join("id_test.pub");
        std::fs::write(
            &pub_path,
            "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAA pubdirect\n",
        )
        .unwrap();
        let entries = entries_from(&[pub_path.to_str().unwrap()]);
        let blobs = collect_identity_blobs(&entries);
        assert_eq!(blobs.len(), 1);
        cleanup(&dir);
    }

    #[test]
    fn is_identity_file_signable_rejects_dotpub_path() {
        // Regression guard for the `.pub`-skip branch in
        // `pubkey_auth` and the libssh `AddIdentity` loop. An
        // IdentityFile that names a `.pub` file is not signable;
        // downstream code must not pass it to
        // `userauth_pubkey_file` or `AddIdentity`.
        let entry = IdentityFileEntry {
            path: "/home/me/.ssh/id_rsa.pub".to_string(),
        };
        assert!(!is_identity_file_signable(&entry));
    }

    #[test]
    fn is_identity_file_signable_accepts_private_key_path() {
        // The common case: private key path without a `.pub`
        // suffix. Must stay signable so `pubkey_auth` actually
        // tries it.
        let entry = IdentityFileEntry {
            path: "/home/me/.ssh/id_rsa".to_string(),
        };
        assert!(is_identity_file_signable(&entry));
    }

    #[test]
    fn is_identity_file_signable_accepts_path_with_pub_in_middle() {
        // Only the `.pub` *suffix* disqualifies a path; an
        // infix `pub` somewhere in the middle of the filename
        // stays signable. This pins the end-suffix semantics
        // against any future temptation to use `.contains(".pub")`.
        let entry = IdentityFileEntry {
            path: "/home/me/.ssh/id_pub_key_2024".to_string(),
        };
        assert!(is_identity_file_signable(&entry));
    }

    #[test]
    fn identities_only_not_set_allows_all() {
        // No identitiesonly in config → None (all keys allowed)
        let config = crate::config::ConfigMap::new();
        assert!(allowed_agent_key_blobs(&config, &[]).is_none());
    }

    #[test]
    fn identities_only_no_disables_filtering() {
        let mut config = crate::config::ConfigMap::new();
        config.insert("identitiesonly".into(), "no".into());
        assert!(allowed_agent_key_blobs(&config, &[]).is_none());
    }

    #[test]
    fn identities_only_yes_returns_matching_blobs() {
        let dir = tmpdir("id_only_match");
        let pub_path = dir.join("id_test.pub");
        std::fs::write(&pub_path, "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAA test\n").unwrap();

        let mut config = crate::config::ConfigMap::new();
        config.insert("identitiesonly".into(), "yes".into());
        let entries = entries_from(&[dir.join("id_test").to_str().unwrap()]);

        let allowed = allowed_agent_key_blobs(&config, &entries);
        assert!(allowed.is_some());
        let blobs = allowed.unwrap();
        assert_eq!(blobs.len(), 1);
        assert_eq!(
            blobs[0],
            base64::engine::general_purpose::STANDARD
                .decode("AAAAB3NzaC1yc2EAAAADAQABAAAA")
                .unwrap()
        );
        cleanup(&dir);
    }

    #[test]
    fn identities_only_value_is_case_insensitive() {
        for value in ["YES", "Yes", " yes ", "yEs"] {
            let mut config = crate::config::ConfigMap::new();
            config.insert("identitiesonly".into(), value.into());
            assert!(
                allowed_agent_key_blobs(&config, &[]).is_some(),
                "value {:?} should enable filtering",
                value
            );
        }
    }

    #[test]
    fn identities_only_yes_no_identity_file_returns_empty() {
        // IdentitiesOnly=yes but no IdentityFile → empty allowed list
        let mut config = crate::config::ConfigMap::new();
        config.insert("identitiesonly".into(), "yes".into());

        let allowed = allowed_agent_key_blobs(&config, &[]);
        assert!(allowed.is_some());
        assert!(allowed.unwrap().is_empty());
    }

    #[test]
    fn identities_only_yes_missing_key_files_returns_empty() {
        // IdentitiesOnly=yes with IdentityFile pointing to nonexistent paths
        let mut config = crate::config::ConfigMap::new();
        config.insert("identitiesonly".into(), "yes".into());
        let entries = entries_from(&["/nonexistent/id_key"]);

        let allowed = allowed_agent_key_blobs(&config, &entries);
        assert!(allowed.is_some());
        assert!(allowed.unwrap().is_empty());
    }
}
