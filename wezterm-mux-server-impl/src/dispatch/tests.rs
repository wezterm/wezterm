use super::*;
use codec::{GetPaneRenderChanges, GetPaneRenderableDimensions, Ping, SendPaste, SetClientId};
use mux::client::ClientId;
use mux::pane::{Pane, PaneId};
use promise::spawn::{spawn_into_main_thread, ScopedExecutor};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;
use termwiz::escape::Action;
use wezterm_term::{Alert, TerminalSize};

async fn recv(stream: &mut Async<UnixStream>) -> anyhow::Result<DecodedPdu> {
    Pdu::decode_async(stream, None).await
}

async fn connect() -> anyhow::Result<Async<UnixStream>> {
    let (server, client) = UnixStream::pair()?;
    let server = Async::new(server)?;
    spawn_into_main_thread(process_async(server, RenderPushPolicy::RenderClientsOnly)).detach();
    let mut client = Async::new(client)?;
    Pdu::Ping(Ping {}).encode_async(&mut client, 1).await?;
    let reply = recv(&mut client).await?;
    assert_eq!(reply.serial, 1);
    assert!(matches!(reply.pdu, Pdu::Pong(_)));
    Ok(client)
}

fn output(mux: &Mux, pane: &dyn Pane, text: &str) {
    pane.perform_actions(vec![Action::PrintString(text.to_owned())]);
    mux.notify(MuxNotification::PaneOutput(pane.pane_id()));
}

async fn check_one_shot_reply(
    client: &mut Async<UnixStream>,
    pane_id: PaneId,
) -> anyhow::Result<()> {
    Pdu::GetPaneRenderableDimensions(GetPaneRenderableDimensions { pane_id })
        .encode_async(client, 2)
        .await?;
    let reply = recv(client).await?;
    assert_eq!(reply.serial, 2, "unexpected PDU: {}", reply.pdu.pdu_name());
    assert!(matches!(
        reply.pdu,
        Pdu::GetPaneRenderableDimensionsResponse(_)
    ));
    Ok(())
}

async fn set_client_id(client: &mut Async<UnixStream>, is_proxy: bool) -> anyhow::Result<()> {
    Pdu::SetClientId(SetClientId {
        client_id: ClientId::new(),
        is_proxy,
    })
    .encode_async(client, 3)
    .await?;
    let reply = recv(client).await?;
    assert_eq!(reply.serial, 3);
    assert!(matches!(reply.pdu, Pdu::UnitResponse(_)));
    Ok(())
}

async fn check_render(
    client: &mut Async<UnixStream>,
    pane_id: PaneId,
    text: &str,
) -> anyhow::Result<()> {
    let update = recv(client).await?;
    assert_eq!(update.serial, 0);
    let Pdu::GetPaneRenderChangesResponse(render) = update.pdu else {
        anyhow::bail!("expected render update, got {}", update.pdu.pdu_name());
    };
    assert_eq!(render.pane_id, pane_id);
    let (lines, _) = render.bonus_lines.extract_data();
    assert!(lines.iter().any(|(_, line)| line.as_str().contains(text)));
    Ok(())
}

#[test]
fn pane_output_only_pushes_to_render_consumers() -> anyhow::Result<()> {
    let mut config = config::Config::default_config();
    config.exit_behavior = config::ExitBehavior::Close;
    config.mux_enable_ssh_agent = false;
    config::use_this_configuration(config);
    let mux = Arc::new(Mux::new(None));
    Mux::set_mux(&mux);
    let executor = ScopedExecutor::new();
    let result = smol::block_on(executor.run(smol::future::race(
        async {
            let (terminal, pane) = mux::termwiztermtab::allocate(
                TerminalSize::default(),
                Arc::new(config::TermConfig::new()),
            );
            let pane_id = pane.pane_id();
            let mut passive = connect().await?;
            let mut renderer = connect().await?;
            let mut mux_client = connect().await?;
            // An SSH mux client registers through a proxy connection.
            set_client_id(&mut mux_client, true).await?;
            set_client_id(&mut mux_client, false).await?;

            // Prior output must be included in the initial render, and a
            // CLI-style dimensions request must receive only its own reply.
            output(&mux, &*pane, "initial");
            check_one_shot_reply(&mut passive, pane_id).await?;
            check_render(&mut mux_client, pane_id, "initial").await?;
            assert!(matches!(recv(&mut mux_client).await?.pdu, Pdu::SetPalette(_)));
            Pdu::GetPaneRenderChanges(GetPaneRenderChanges { pane_id })
                .encode_async(&mut renderer, 2)
                .await?;
            check_render(&mut renderer, pane_id, "initial").await?;
            assert!(matches!(recv(&mut renderer).await?.pdu, Pdu::SetPalette(_)));
            let reply = recv(&mut renderer).await?;
            assert_eq!(reply.serial, 2);
            assert!(matches!(reply.pdu, Pdu::LivenessResponse(reply) if reply.is_alive));

            // A proxy handshake and one-shot input must not subscribe the
            // connection to future render pushes.
            set_client_id(&mut passive, true).await?;
            Pdu::SendPaste(SendPaste {
                pane_id,
                data: "paste".to_string(),
            })
            .encode_async(&mut passive, 4)
            .await?;
            loop {
                let reply = recv(&mut passive).await?;
                if reply.serial == 4 {
                    assert!(matches!(reply.pdu, Pdu::UnitResponse(_)));
                    break;
                }
                assert_eq!(reply.serial, 0);
            }

            output(&mux, &*pane, " more");
            check_render(&mut renderer, pane_id, "initial more").await?;
            check_render(&mut mux_client, pane_id, "initial more").await?;
            check_one_shot_reply(&mut passive, pane_id).await?;

            // An identified client uses pushes to discover a new pane.
            // The explicit request on the other connection only covers its pane.
            let (new_terminal, new_pane) = mux::termwiztermtab::allocate(
                TerminalSize::default(),
                Arc::new(config::TermConfig::new()),
            );
            let new_pane_id = new_pane.pane_id();
            output(&mux, &*new_pane, "new pane");
            check_render(&mut mux_client, new_pane_id, "new pane").await?;
            assert!(matches!(recv(&mut mux_client).await?.pdu, Pdu::SetPalette(_)));
            mux.notify(MuxNotification::Alert {
                pane_id: new_pane_id,
                alert: Alert::Bell,
            });
            let update = recv(&mut mux_client).await?;
            assert!(matches!(
                update.pdu,
                Pdu::NotifyAlert(alert) if alert.pane_id == new_pane_id && alert.alert == Alert::Bell
            ));
            check_one_shot_reply(&mut renderer, new_pane_id).await?;
            check_one_shot_reply(&mut passive, new_pane_id).await?;

            // Close the pipes and wait for the readers to finish using the mux.
            for (terminal, pane_id) in [(terminal, pane_id), (new_terminal, new_pane_id)] {
                drop(terminal);
                let removal = recv(&mut passive).await?;
                assert!(matches!(removal.pdu, Pdu::PaneRemoved(pane) if pane.pane_id == pane_id));
            }
            Ok(())
        },
        async {
            smol::Timer::after(Duration::from_secs(5)).await;
            anyhow::bail!("timed out waiting for mux packets");
        },
    )));
    // Dropping the sessions unregisters their client IDs from the mux.
    drop(executor);
    Mux::shutdown();
    result
}
