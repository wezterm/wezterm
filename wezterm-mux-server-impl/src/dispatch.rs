use crate::sessionhandler::{PduSender, SessionHandler};
use anyhow::Context;
use async_ossl::AsyncSslStream;
use codec::{DecodedPdu, Pdu};
use futures::FutureExt;
use mux::pane::PaneId;
use mux::tab::TabId;
use mux::window::WindowId;
use mux::{Mux, MuxNotification};
use smol::prelude::*;
use smol::Async;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use wezterm_term::Alert;
use wezterm_uds::UnixStream;

#[cfg(unix)]
pub trait AsRawDesc: std::os::unix::io::AsRawFd + std::os::fd::AsFd {}
#[cfg(windows)]
pub trait AsRawDesc: std::os::windows::io::AsRawSocket + std::os::windows::io::AsSocket {}

impl AsRawDesc for UnixStream {}
impl AsRawDesc for AsyncSslStream {}

#[derive(Debug)]
enum Item {
    Notif(MuxNotification),
    // Boxed so that the far more numerous Notif items do not each occupy
    // the size of the largest Pdu in the queue.
    WritePdu(Box<DecodedPdu>),
    Readable,
}

/// A queued notification whose newest value replaces any earlier one.
/// SetUserVar is deliberately absent: every change fires the
/// user-var-changed event in the GUI, so each one must be delivered.
///
/// Because a coalesced entry keeps its original queue position, its value
/// can be newer than a ListPanes response queued after it. The client then
/// applies the older title from that response, and echoes it back to the
/// server, until the next title change queues a fresh entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Slot {
    PaneIconTitle(PaneId),
    PaneWindowTitle(PaneId),
    PaneTabTitle(PaneId),
    PaneProgress(PaneId),
    WindowTitle(WindowId),
    TabTitle(TabId),
}

impl Slot {
    fn for_notification(notification: &MuxNotification) -> Option<Self> {
        match notification {
            MuxNotification::Alert { pane_id, alert } => match alert {
                Alert::IconTitleChanged(_) => Some(Self::PaneIconTitle(*pane_id)),
                Alert::WindowTitleChanged(_) => Some(Self::PaneWindowTitle(*pane_id)),
                Alert::TabTitleChanged(_) => Some(Self::PaneTabTitle(*pane_id)),
                Alert::Progress(_) => Some(Self::PaneProgress(*pane_id)),
                _ => None,
            },
            MuxNotification::WindowTitleChanged { window_id, .. } => {
                Some(Self::WindowTitle(*window_id))
            }
            MuxNotification::TabTitleChanged { tab_id, .. } => Some(Self::TabTitle(*tab_id)),
            _ => None,
        }
    }
}

#[derive(Default)]
struct PendingState {
    output: HashSet<PaneId>,
    latest: HashMap<Slot, MuxNotification>,
}

/// Tracks the notifications that are queued for this session but not yet
/// taken by its loop. A busy pane emits PaneOutput every few milliseconds,
/// and a TUI may set its title on every frame. While the client is slow to
/// read, the loop stops draining its unbounded queue, so without
/// coalescing the queue grows for as long as the client stalls.
///
/// One queued PaneOutput per pane is sufficient because the resulting push
/// computes every change since the session last computed that pane's
/// changes. For notifications that carry replaceable state, the queue
/// holds one entry per slot and the loop sends the newest value.
#[derive(Clone, Default)]
struct PendingNotifications(Arc<Mutex<PendingState>>);

impl PendingNotifications {
    /// Called by the session loop for every notification it takes.
    /// Clears the PaneOutput mark before the push computes its changes,
    /// so that output arriving afterwards queues a fresh notification,
    /// and returns the newest value for a coalesced slot.
    fn take(&self, notification: MuxNotification) -> MuxNotification {
        let mut state = self.0.lock().unwrap();
        if let MuxNotification::PaneOutput(pane_id) = &notification {
            state.output.remove(pane_id);
        }
        match Slot::for_notification(&notification) {
            Some(slot) => state.latest.remove(&slot).unwrap_or(notification),
            None => notification,
        }
    }
}

/// The mux subscriber body for a session: queues the notification unless
/// an equivalent one is already queued. Returns false once the session
/// has gone away, which unsubscribes it.
fn forward(
    notification: MuxNotification,
    pending: &PendingNotifications,
    tx: &smol::channel::Sender<Item>,
) -> bool {
    {
        let mut state = pending.0.lock().unwrap();
        let already_queued = match &notification {
            MuxNotification::PaneOutput(pane_id) => !state.output.insert(*pane_id),
            _ => match Slot::for_notification(&notification) {
                Some(slot) => state.latest.insert(slot, notification.clone()).is_some(),
                None => false,
            },
        };
        if already_queued {
            return !tx.is_closed();
        }
    }
    tx.try_send(Item::Notif(notification)).is_ok()
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
                .try_send(Item::WritePdu(Box::new(pdu)))
                .map_err(|e| anyhow::anyhow!("{:?}", e))
        }
    });
    let mut handler = SessionHandler::new(pdu_sender);

    let pending = PendingNotifications::default();

    {
        let mux = Mux::get();
        let tx = item_tx.clone();
        let pending = pending.clone();
        mux.subscribe(move |n| forward(n, &pending, &tx));
    }

    loop {
        let rx_msg = item_rx.recv();
        let wait_for_read = stream.readable().map(|_| Ok(Item::Readable));

        let item = match smol::future::or(rx_msg, wait_for_read).await {
            Ok(Item::Notif(notification)) => Ok(Item::Notif(pending.take(notification))),
            item => item,
        };

        match item {
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
            Ok(Item::Notif(MuxNotification::PaneFocused(pane_id))) => {
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
    use wezterm_term::Progress;

    fn output(pane_id: PaneId) -> MuxNotification {
        MuxNotification::PaneOutput(pane_id)
    }

    fn alert(pane_id: PaneId, alert: Alert) -> MuxNotification {
        MuxNotification::Alert { pane_id, alert }
    }

    fn title(pane_id: PaneId, title: &str) -> MuxNotification {
        alert(pane_id, Alert::WindowTitleChanged(title.to_string()))
    }

    fn user_var(pane_id: PaneId, name: &str, value: &str) -> MuxNotification {
        alert(
            pane_id,
            Alert::SetUserVar {
                name: name.to_string(),
                value: value.to_string(),
            },
        )
    }

    /// Takes the next queued item the way the session loop does.
    fn take(rx: &smol::channel::Receiver<Item>, pending: &PendingNotifications) -> MuxNotification {
        match rx.try_recv().expect("an item is queued") {
            Item::Notif(n) => pending.take(n),
            item => panic!("unexpected {:?}", item),
        }
    }

    #[test]
    fn queued_items_stay_small() {
        assert!(std::mem::size_of::<Item>() <= 128);
    }

    #[test]
    fn pane_output_is_queued_once_per_pane() {
        let (tx, rx) = smol::channel::unbounded();
        let pending = PendingNotifications::default();

        for _ in 0..100 {
            assert!(forward(output(1), &pending, &tx));
            assert!(forward(output(2), &pending, &tx));
        }
        assert_eq!(rx.len(), 2);

        assert!(matches!(
            take(&rx, &pending),
            MuxNotification::PaneOutput(1)
        ));
        assert!(forward(output(1), &pending, &tx));
        assert!(forward(output(2), &pending, &tx));
        assert_eq!(
            rx.len(),
            2,
            "pane 1 queues again once taken; pane 2 does not"
        );
    }

    #[test]
    fn replaceable_alerts_deliver_the_newest_value() {
        let (tx, rx) = smol::channel::unbounded();
        let pending = PendingNotifications::default();

        for i in 0..50 {
            assert!(forward(title(1, &format!("frame {}", i)), &pending, &tx));
            assert!(forward(
                alert(1, Alert::Progress(Progress::Percentage(i))),
                &pending,
                &tx
            ));
        }
        assert!(forward(title(2, "other pane"), &pending, &tx));
        assert_eq!(rx.len(), 3);

        let mut taken = vec![];
        while !rx.is_empty() {
            match take(&rx, &pending) {
                MuxNotification::Alert { pane_id, alert } => taken.push((pane_id, alert)),
                n => panic!("unexpected {:?}", n),
            }
        }
        assert_eq!(
            taken,
            vec![
                (1, Alert::WindowTitleChanged("frame 49".to_string())),
                (1, Alert::Progress(Progress::Percentage(49))),
                (2, Alert::WindowTitleChanged("other pane".to_string())),
            ]
        );

        assert!(forward(title(1, "after take"), &pending, &tx));
        assert_eq!(rx.len(), 1, "a taken slot queues again");
    }

    #[test]
    fn window_and_tab_titles_deliver_the_newest_value() {
        let (tx, rx) = smol::channel::unbounded();
        let pending = PendingNotifications::default();

        for i in 0..50 {
            let title = format!("frame {}", i);
            assert!(forward(
                MuxNotification::WindowTitleChanged {
                    window_id: 0,
                    title: title.clone(),
                },
                &pending,
                &tx
            ));
            assert!(forward(
                MuxNotification::TabTitleChanged { tab_id: 3, title },
                &pending,
                &tx
            ));
        }
        assert_eq!(rx.len(), 2);

        match take(&rx, &pending) {
            MuxNotification::WindowTitleChanged { window_id, title } => {
                assert_eq!((window_id, title.as_str()), (0, "frame 49"));
            }
            n => panic!("unexpected {:?}", n),
        }
        match take(&rx, &pending) {
            MuxNotification::TabTitleChanged { tab_id, title } => {
                assert_eq!((tab_id, title.as_str()), (3, "frame 49"));
            }
            n => panic!("unexpected {:?}", n),
        }
    }

    #[test]
    fn one_shot_alerts_and_user_vars_are_not_coalesced() {
        let (tx, rx) = smol::channel::unbounded();
        let pending = PendingNotifications::default();

        for _ in 0..3 {
            assert!(forward(alert(1, Alert::Bell), &pending, &tx));
        }
        for value in ["+1", "+1", "-1"] {
            assert!(forward(user_var(1, "ZEN_MODE", value), &pending, &tx));
        }
        assert_eq!(rx.len(), 6);

        let user_vars: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|item| match item {
                Item::Notif(MuxNotification::Alert {
                    alert: Alert::SetUserVar { value, .. },
                    ..
                }) => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(user_vars, vec!["+1", "+1", "-1"]);
    }

    #[test]
    fn closed_session_unsubscribes_even_when_coalesced() {
        let (tx, rx) = smol::channel::unbounded();
        let pending = PendingNotifications::default();

        assert!(forward(output(1), &pending, &tx));
        assert!(forward(title(1, "a"), &pending, &tx));
        drop(rx);

        assert!(!forward(output(1), &pending, &tx));
        assert!(!forward(title(1, "b"), &pending, &tx));
        assert!(!forward(output(2), &pending, &tx));
        assert!(!forward(MuxNotification::Empty, &pending, &tx));
    }
}
