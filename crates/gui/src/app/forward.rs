//! Forwarding a message: the dialog that picks where to, and the request.
//!
//! The daemon relays the stored message as it is — media included, nothing
//! downloaded or uploaded again — and writes each copy to its chat as it
//! goes, so the copies arrive in the timeline the way any send does.

use gpui::{AppContext as _, Context, Entity, Subscription, Window};

use super::WhatsAppApp;
use super::notices::Tone;
use crate::components::{ForwardEvent, ForwardPicker, ForwardTarget};

/// The dialog while it is up, and what it is forwarding.
pub(super) struct Forwarding {
    pub jid: String,
    pub message_id: String,
    pub picker: Entity<ForwardPicker>,
    _events: Subscription,
}

impl WhatsAppApp {
    /// Open the dialog for one message of the open chat.
    pub(super) fn begin_forward(
        &mut self,
        message_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.forward.is_some() || self.delete_confirmation.is_some() {
            return;
        }
        let Some(jid) = self.selected_chat.clone() else {
            return;
        };
        let Some(preview) = self
            .find_chat(&jid)
            .and_then(|chat| chat.messages.iter().find(|m| m.id == message_id))
            .map(|message| message.preview_text().chars().take(160).collect::<String>())
        else {
            return;
        };
        let targets = self.forward_targets();
        let picker = cx.new(|cx| ForwardPicker::new(targets, preview, window, cx));
        let events = cx.subscribe(&picker, |app, _, event: &ForwardEvent, cx| match event {
            ForwardEvent::Cancel => {
                app.cancel_forward(cx);
            }
            ForwardEvent::Send(targets) => app.finish_forward(targets.clone(), cx),
        });
        self.forward = Some(Forwarding {
            jid,
            message_id: message_id.to_string(),
            picker,
            _events: events,
        });
        cx.notify();
    }

    /// Close the dialog without sending. Whether there was one to close.
    pub(super) fn cancel_forward(&mut self, cx: &mut Context<Self>) -> bool {
        if self.forward.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    /// The chats a message can go to, most recent first: every chat but the
    /// status broadcast and channels, which only their admins post in.
    fn forward_targets(&self) -> Vec<ForwardTarget> {
        let mut chats: Vec<_> = self
            .chats
            .iter()
            .filter(|chat| !chat.is_status && !chat.jid.ends_with("@newsletter"))
            .filter(|chat| chat.jid != oxidezap_core::STATUS_BROADCAST_JID)
            .collect();
        chats.sort_by_key(|chat| std::cmp::Reverse(chat.last_message_time));
        chats
            .into_iter()
            .map(|chat| ForwardTarget {
                jid: chat.jid.clone(),
                name: if self.is_own_number(&chat.jid) {
                    format!("{} ({})", chat.name, crate::tr!(You))
                } else {
                    chat.name.clone()
                },
                is_group: chat.is_group,
                avatar: chat.avatar_cache_key.clone(),
            })
            .collect()
    }

    fn finish_forward(&mut self, targets: Vec<String>, cx: &mut Context<Self>) {
        let Some(forward) = self.forward.take() else {
            return;
        };
        cx.notify();
        let Some(client) = self.client.as_ref() else {
            self.notify_user(crate::tr!(ForwardFailed), Tone::Problem, cx);
            return;
        };
        let answer = client.forward_message(forward.jid, forward.message_id, targets);
        cx.spawn(async move |app: gpui::WeakEntity<Self>, cx| {
            let result = answer.await;
            let _ = app.update(cx, |app, cx| match result {
                Ok(Ok(())) => app.notify_user(crate::tr!(MessageForwarded), Tone::Info, cx),
                Ok(Err(failure)) => app.notify_user(
                    super::what_went_wrong(crate::tr!(ForwardFailed), &failure),
                    Tone::Problem,
                    cx,
                ),
                Err(_) => app.notify_user(crate::tr!(ForwardFailed), Tone::Problem, cx),
            });
        })
        .detach();
    }
}
