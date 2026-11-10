#![cfg(windows)]

use crate::{ToastDuration, ToastNotification as TN};
use xml::escape::escape_str_pcdata;

use windows::core::{Error as WinError, IInspectable, Interface, HSTRING};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::Win32::Foundation::E_POINTER;
use windows::UI::Notifications::{
    ToastActivatedEventArgs, ToastNotification, ToastNotificationManager,
};

fn unwrap_arg<T>(a: &Option<T>) -> Result<&T, WinError> {
    match a {
        Some(t) => Ok(t),
        None => Err(WinError::new(E_POINTER, HSTRING::from("option is none"))),
    }
}

const APP_ID: &str = "org.wezfurlong.wezterm";

/// Tags are scoped to a group. Ours is specific to this process, so that
/// the same tag used by a different wezterm process refers to a different
/// notification.
fn tag_group() -> HSTRING {
    HSTRING::from(format!("wz-{}", std::process::id()))
}

fn show_notif_impl(toast: TN) -> Result<(), Box<dyn std::error::Error>> {
    let xml = XmlDocument::new()?;

    let url_actions = if toast.url.is_some() {
        r#"
        <actions>
           <action content="Show" arguments="show" />
        </actions>
        "#
    } else {
        ""
    };

    let duration = match toast.duration {
        ToastDuration::Short => "short",
        ToastDuration::Long => "long",
    };

    let attribution = match toast.attribution.as_ref() {
        Some(attribution) => format!(
            r#"<text placement="attribution">{}</text>"#,
            escape_str_pcdata(attribution)
        ),
        None => String::new(),
    };

    let audio = if toast.silent {
        r#"<audio silent="true" />"#
    } else {
        ""
    };

    xml.LoadXml(HSTRING::from(format!(
        r#"<toast duration="{}">
        <visual>
            <binding template="ToastGeneric">
                <text>{}</text>
                <text>{}</text>
                {}
            </binding>
        </visual>
        {}
        {}
    </toast>"#,
        duration,
        escape_str_pcdata(&toast.title),
        escape_str_pcdata(&toast.message),
        attribution,
        audio,
        url_actions
    )))?;

    let notif = ToastNotification::CreateToastNotification(xml)?;

    if let Some(tag) = toast.tag.as_ref() {
        notif.SetTag(HSTRING::from(tag.as_str()))?;
        notif.SetGroup(tag_group())?;
    }

    notif.Activated(TypedEventHandler::new(
        move |_: &Option<ToastNotification>, result: &Option<IInspectable>| {
            // let myself = unwrap_arg(myself)?;
            let result = unwrap_arg(result)?.cast::<ToastActivatedEventArgs>()?;

            let args = result.Arguments()?;

            if args == "show" {
                if let Some(url) = toast.url.as_ref() {
                    wezterm_open_url::open_url(url);
                }
            } else if args == "" {
                // We don't set the `launch` attribute, so a click on the
                // body of the notification, rather than on one of its
                // action buttons, arrives with empty arguments.
                if let Some(on_click) = toast.on_click.as_ref() {
                    on_click.call();
                }
            }

            Ok(())
        },
    ))?;

    /*
    notif.dismissed(TypedEventHandler::new(|sender, result| {
        log::info!("dismissed {:?}", result);
        Ok(())
    }))?;

    notif.failed(TypedEventHandler::new(|sender, result| {
        log::warn!("toasts are disabled {:?}", result);
        Ok(())
    }))?;
    */

    let notifier = ToastNotificationManager::CreateToastNotifierWithId(HSTRING::from(APP_ID))?;

    notifier.Show(&notif)?;

    Ok(())
}

pub fn show_notif(notif: TN) -> Result<(), Box<dyn std::error::Error>> {
    // We need to be in a different thread from the caller
    // in case we get called in the guts of a windows message
    // loop dispatch and are unable to pump messages
    std::thread::spawn(move || {
        if let Err(err) = show_notif_impl(notif) {
            log::error!("Failed to show toast notification: {:#}", err);
        }
    });

    Ok(())
}

fn dismiss_notif_impl(tag: &str) -> Result<(), WinError> {
    ToastNotificationManager::History()?.RemoveGroupedTagWithId(
        HSTRING::from(tag),
        tag_group(),
        HSTRING::from(APP_ID),
    )
}

pub fn dismiss_notif(tag: &str) {
    let tag = tag.to_string();
    // Use a separate thread for the same reason as in show_notif
    std::thread::spawn(move || {
        if let Err(err) = dismiss_notif_impl(&tag) {
            log::debug!("Failed to dismiss toast notification {}: {:#}", tag, err);
        }
    });
}
