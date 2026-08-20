use crate::sessionhandler::{PduSender, SessionHandler};
use anyhow::Context;
use async_ossl::AsyncSslStream;
use codec::{DecodedPdu, Pdu};
use futures::FutureExt;
use mux::client::ClientId;
use mux::{Mux, MuxNotification};
use smol::prelude::*;
use smol::Async;
use std::sync::Arc;
use wezterm_uds::UnixStream;

#[cfg(unix)]
pub trait AsRawDesc: std::os::unix::io::AsRawFd + std::os::fd::AsFd {}
#[cfg(windows)]
pub trait AsRawDesc: std::os::windows::io::AsRawSocket + std::os::windows::io::AsSocket {}

impl AsRawDesc for UnixStream {}
impl AsRawDesc for AsyncSslStream {}

/// Should a PaneFocused notification attributed to `origin` be sent to the
/// client on the other end of this session?
///
/// Not if that client is the one who asked for it: it moved its own view the
/// moment the user pressed the key and doesn't need our permission. Telling it
/// again would be worse than redundant, because over a slow link the user has
/// often moved on to another tab by the time the notification lands, and
/// applying it would drag them back to where they were.
fn should_forward_pane_focused(
    origin: &Option<Arc<ClientId>>,
    session_client_id: &Option<Arc<ClientId>>,
) -> bool {
    match (origin, session_client_id) {
        (Some(origin), Some(session_client_id)) => origin != session_client_id,
        // A change the mux made by itself, or a session that hasn't told us
        // who it is yet: nobody here is responsible for it, so pass it on.
        _ => true,
    }
}

#[derive(Debug)]
enum Item {
    Notif(MuxNotification),
    WritePdu(DecodedPdu),
    Readable,
}

pub async fn process<T>(stream: T) -> anyhow::Result<()>
where
    T: 'static,
    T: std::io::Read,
    T: std::io::Write,
    T: AsRawDesc,
    T: std::fmt::Debug,
    T: async_io::IoSafe,
{
    let stream = smol::Async::new(stream)?;
    process_async(stream).await
}

pub async fn process_async<T>(mut stream: Async<T>) -> anyhow::Result<()>
where
    T: 'static,
    T: std::io::Read,
    T: std::io::Write,
    T: std::fmt::Debug,
    T: async_io::IoSafe,
{
    log::trace!("process_async called");

    let (item_tx, item_rx) = smol::channel::unbounded::<Item>();

    let pdu_sender = PduSender::new({
        let item_tx = item_tx.clone();
        move |pdu| {
            item_tx
                .try_send(Item::WritePdu(pdu))
                .map_err(|e| anyhow::anyhow!("{:?}", e))
        }
    });
    let mut handler = SessionHandler::new(pdu_sender);

    {
        let mux = Mux::get();
        let tx = item_tx.clone();
        mux.subscribe(move |n| tx.try_send(Item::Notif(n)).is_ok());
    }

    loop {
        let rx_msg = item_rx.recv();
        let wait_for_read = stream.readable().map(|_| Ok(Item::Readable));

        match smol::future::or(rx_msg, wait_for_read).await {
            Ok(Item::Readable) => {
                let decoded = match Pdu::decode_async(&mut stream, None).await {
                    Ok(data) => data,
                    Err(err) => {
                        if let Some(err) = err.root_cause().downcast_ref::<std::io::Error>() {
                            if err.kind() == std::io::ErrorKind::UnexpectedEof {
                                // Client disconnected: no need to make a noise
                                return Ok(());
                            }
                        }
                        return Err(err).context("reading Pdu from client");
                    }
                };
                handler.process_one(decoded);
            }
            Ok(Item::WritePdu(decoded)) => {
                match decoded.pdu.encode_async(&mut stream, decoded.serial).await {
                    Ok(()) => {}
                    Err(err) => {
                        if let Some(err) = err.root_cause().downcast_ref::<std::io::Error>() {
                            if err.kind() == std::io::ErrorKind::BrokenPipe {
                                // Client disconnected: no need to make a noise
                                return Ok(());
                            }
                        }
                        return Err(err).context("encoding PDU to client");
                    }
                };
                match stream.flush().await {
                    Ok(()) => {}
                    Err(err) => {
                        if err.kind() == std::io::ErrorKind::BrokenPipe {
                            // Client disconnected: no need to make a noise
                            return Ok(());
                        }
                        return Err(err).context("flushing PDU to client");
                    }
                }
            }
            Ok(Item::Notif(MuxNotification::PaneOutput(pane_id))) => {
                handler.schedule_pane_push(pane_id);
            }
            Ok(Item::Notif(MuxNotification::PaneAdded(_pane_id))) => {}
            Ok(Item::Notif(MuxNotification::PaneRemoved(pane_id))) => {
                Pdu::PaneRemoved(codec::PaneRemoved { pane_id })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::Alert { pane_id, alert })) => {
                {
                    let per_pane = handler.per_pane(pane_id);
                    let mut per_pane = per_pane.lock().unwrap();
                    per_pane.notifications.push(alert);
                }
                handler.schedule_pane_push(pane_id);
            }
            Ok(Item::Notif(MuxNotification::SaveToDownloads { .. })) => {}
            Ok(Item::Notif(MuxNotification::AssignClipboard {
                pane_id,
                selection,
                clipboard,
            })) => {
                Pdu::SetClipboard(codec::SetClipboard {
                    pane_id,
                    clipboard,
                    selection,
                })
                .encode_async(&mut stream, 0)
                .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::TabAddedToWindow { tab_id, window_id })) => {
                Pdu::TabAddedToWindow(codec::TabAddedToWindow { tab_id, window_id })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::WindowRemoved(_window_id))) => {}
            Ok(Item::Notif(MuxNotification::WindowCreated(_window_id))) => {}
            Ok(Item::Notif(MuxNotification::WindowInvalidated(_window_id))) => {}
            Ok(Item::Notif(MuxNotification::WindowWorkspaceChanged(window_id))) => {
                let workspace = {
                    let mux = Mux::get();
                    mux.get_window(window_id)
                        .map(|w| w.get_workspace().to_string())
                };
                if let Some(workspace) = workspace {
                    Pdu::WindowWorkspaceChanged(codec::WindowWorkspaceChanged {
                        window_id,
                        workspace,
                    })
                    .encode_async(&mut stream, 0)
                    .await?;
                    stream.flush().await.context("flushing PDU to client")?;
                }
            }
            Ok(Item::Notif(MuxNotification::PaneFocused { pane_id, origin })) => {
                if !should_forward_pane_focused(&origin, &handler.client_id()) {
                    continue;
                }
                Pdu::PaneFocused(codec::PaneFocused { pane_id })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::TabResized(tab_id))) => {
                Pdu::TabResized(codec::TabResized { tab_id })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::TabTitleChanged { tab_id, title })) => {
                Pdu::TabTitleChanged(codec::TabTitleChanged { tab_id, title })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::WindowTitleChanged { window_id, title })) => {
                Pdu::WindowTitleChanged(codec::WindowTitleChanged { window_id, title })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::WorkspaceRenamed {
                old_workspace,
                new_workspace,
            })) => {
                Pdu::RenameWorkspace(codec::RenameWorkspace {
                    old_workspace,
                    new_workspace,
                })
                .encode_async(&mut stream, 0)
                .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::ActiveWorkspaceChanged(_))) => {}
            Ok(Item::Notif(MuxNotification::Empty)) => {}
            Err(err) => {
                log::error!("process_async Err {}", err);
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// A client that asks the mux to move the focus has already moved its own
    /// view to match, so the notification that results from its request must
    /// not be sent back to it. Everyone else still needs to hear about it.
    #[test]
    fn pane_focused_is_not_echoed_to_the_client_that_asked() {
        let me = Arc::new(ClientId::new());
        let someone_else = Arc::new(ClientId::new());

        // The echo of our own request.
        assert!(!should_forward_pane_focused(
            &Some(me.clone()),
            &Some(me.clone())
        ));

        // Another attached client, or a `wezterm cli activate-pane` run by
        // some script: this is news to us and we want it.
        assert!(should_forward_pane_focused(
            &Some(someone_else),
            &Some(me.clone())
        ));

        // A focus change the mux made by itself, on nobody's behalf.
        assert!(should_forward_pane_focused(&None, &Some(me)));

        // A session that hasn't identified itself yet can't be the one that
        // asked, so it hears about everything.
        assert!(should_forward_pane_focused(
            &Some(Arc::new(ClientId::new())),
            &None
        ));
    }
}
