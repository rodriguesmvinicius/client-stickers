//! The app's half of a composer link card: the composer finds the link and
//! shows the card; asking the daemon for it is here, because the app holds
//! the connection.

use gpui::Context;

use super::WhatsAppApp;

impl WhatsAppApp {
    /// The composer settled on a link: ask the daemon for its card and hand
    /// it back. A failure is no card — the message goes as typed, as it
    /// would from a phone that could not reach the page.
    pub(super) fn fetch_link_preview(&mut self, link: String, cx: &mut Context<Self>) {
        let (Some(client), Some(input)) = (&self.client, self.input_area.clone()) else {
            return;
        };
        let answer = client.link_preview(crate::components::fetchable_link(&link));
        cx.spawn(async move |_app: gpui::WeakEntity<Self>, cx| {
            let preview = match answer.await {
                Ok(Ok(preview)) => preview,
                Ok(Err(detail)) => {
                    log::info!("no link preview: {detail}");
                    None
                }
                Err(_) => None,
            };
            input.update(cx, |view, cx| view.link_preview_arrived(&link, preview, cx));
        })
        .detach();
    }
}
