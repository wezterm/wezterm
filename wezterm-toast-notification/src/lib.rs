mod dbus;
mod macos;
mod windows;

use std::sync::Arc;

/// How long a notification stays on screen before the system moves it
/// out of the way.
/// This is currently only respected on Windows, where the system defines
/// the two durations: `Short` is around 5 seconds and `Long` is around
/// 25 seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToastDuration {
    Short,
    #[default]
    Long,
}

/// A callback that is invoked when the body of the notification is clicked.
/// It may be called from an arbitrary thread, so it should hand off any
/// real work to the appropriate thread.
#[derive(Clone)]
pub struct ClickHandler(Arc<dyn Fn() + Send + Sync>);

impl ClickHandler {
    pub fn new<F: Fn() + Send + Sync + 'static>(func: F) -> Self {
        Self(Arc::new(func))
    }

    pub fn call(&self) {
        (self.0)()
    }
}

impl std::fmt::Debug for ClickHandler {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.debug_struct("ClickHandler").finish()
    }
}

#[derive(Debug, Clone)]
pub struct ToastNotification {
    pub title: String,
    pub message: String,
    pub url: Option<String>,
    pub timeout: Option<std::time::Duration>,
    /// Currently only respected on Windows; see `ToastDuration`.
    pub duration: ToastDuration,
    /// Called when the body of the notification is clicked.
    /// Currently only respected on Windows.
    pub on_click: Option<ClickHandler>,
    /// A less prominent line of text that is shown below the message,
    /// intended to say where the notification came from.
    /// Currently only respected on Windows.
    pub attribution: Option<String>,
    /// If true, the system doesn't play its notification sound.
    /// Currently only respected on Windows.
    pub silent: bool,
    /// Identifies what the notification is about. Showing a notification
    /// replaces the earlier notification that has the same tag, if it
    /// hasn't been dismissed yet, and `dismiss` removes it.
    /// Currently only respected on Windows.
    pub tag: Option<String>,
}

impl ToastNotification {
    pub fn show(self) {
        show(self)
    }
}

#[cfg(windows)]
use crate::windows as backend;
#[cfg(all(not(target_os = "macos"), not(windows)))]
use dbus as backend;
#[cfg(target_os = "macos")]
use macos as backend;

mod nop {
    use super::*;

    #[allow(dead_code)]
    pub fn show_notif(_: ToastNotification) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}

pub fn show(notif: ToastNotification) {
    if let Err(err) = backend::show_notif(notif) {
        log::error!("Failed to show notification: {}", err);
    }
}

/// Removes the notification that was shown with the specified tag, both
/// from the screen and from the notification center.
/// It is not an error if there is no such notification.
/// Currently only has an effect on Windows.
pub fn dismiss(tag: &str) {
    #[cfg(windows)]
    crate::windows::dismiss_notif(tag);
    #[cfg(not(windows))]
    let _ = tag;
}

pub fn persistent_toast_notification_with_click_to_open_url(title: &str, message: &str, url: &str) {
    show(ToastNotification {
        title: title.to_string(),
        message: message.to_string(),
        url: Some(url.to_string()),
        timeout: None,
        duration: ToastDuration::default(),
        on_click: None,
        attribution: None,
        silent: false,
        tag: None,
    });
}

pub fn persistent_toast_notification(title: &str, message: &str) {
    show(ToastNotification {
        title: title.to_string(),
        message: message.to_string(),
        url: None,
        timeout: None,
        duration: ToastDuration::default(),
        on_click: None,
        attribution: None,
        silent: false,
        tag: None,
    });
}

#[cfg(target_os = "macos")]
pub use macos::initialize as macos_initialize;
