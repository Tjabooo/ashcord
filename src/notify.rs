//! Desktop notifications (D-Bus on Linux, toasts on Windows, Notification Center on macOS).

pub fn desktop(title: &str, body: &str) {
    tracing::info!("notify: {title}: {body}");
    if std::env::var_os("ASHCORD_NO_NOTIFY").is_some() {
        return;
    }
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        if let Err(e) = notify_rust::Notification::new()
            .appname("Ashcord")
            .summary(&title)
            .body(&body)
            .show()
        {
            tracing::warn!("notification failed: {e}");
        }
    });
}
