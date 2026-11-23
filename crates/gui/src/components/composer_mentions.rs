//! `@`-mentions in the composer: typing `@` in a group lists its members,
//! narrowed by what follows, and picking one puts an atomic token in the text.
//!
//! The token carries the wire form as its text — `@` plus the member's user
//! part, which is how WhatsApp spells a mention inside a message — and draws
//! the member's name as its label. So the field's text is already what is
//! sent, and the name is what the person sees while writing it; the bubble
//! drawn before the send comes back gets the names, like the one the store
//! hands back later.
//!
//! Who can be mentioned is the app's to say: it holds the group's roster. The
//! composer is told the candidates whenever the chat or its roster changes.

use gpui::{App, Entity, IntoElement, ParentElement, SharedString, Styled, div, prelude::*};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{InlineToken, InputContent};

use super::InputAreaView;
use crate::theme::Metrics;

/// What a mention token's id starts with, before the JID: the composer's
/// other tokens are emoji, and the two must never be mistaken for each other.
const TOKEN_PREFIX: &str = "mention:";
/// How many characters after `@` are still a name being looked up. Past this
/// the `@` is something else — an address, a handle in prose.
const QUERY_MAX: usize = 32;
/// How many rows the list shows before it scrolls.
pub(super) const VISIBLE: usize = 6;

/// One person who can be mentioned here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionCandidate {
    pub jid: String,
    /// What the list and the token show.
    pub name: String,
    /// A second line: the number, when the name is not already it.
    pub detail: Option<String>,
}

impl MentionCandidate {
    /// The member's user part: what follows `@` in the sent text.
    fn user(&self) -> &str {
        self.jid.split('@').next().unwrap_or_default()
    }
}

/// The `@` being typed and the people it matches.
#[derive(Clone, Debug)]
pub(super) struct MentionQuery {
    /// Where the `@` is, as a byte offset into the text.
    pub at: usize,
    pub matches: Vec<MentionCandidate>,
    pub selected: usize,
}

/// The `@`-word the cursor is at the end of, if it is one: its offset and what
/// follows the `@`. `tokens` are the ranges already taken by tokens, where an
/// `@` is part of a finished mention rather than a new one.
pub(super) fn query_at(
    text: &str,
    cursor: usize,
    tokens: &[std::ops::Range<usize>],
) -> Option<(usize, String)> {
    let before = text.get(..cursor)?;
    let at = before.rfind('@')?;
    if tokens
        .iter()
        .any(|range| range.start <= at && at < range.end)
    {
        return None;
    }
    // An `@` inside a word is an address, not a mention.
    if before[..at]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace() && !matches!(c, '(' | '[' | '"' | '\''))
    {
        return None;
    }
    let query = &before[at + 1..];
    if query.contains('\n') || query.starts_with(' ') || query.chars().count() > QUERY_MAX {
        return None;
    }
    if tokens
        .iter()
        .any(|range| at < range.start && range.start < cursor)
    {
        return None;
    }
    Some((at, query.to_string()))
}

/// `s` lowercased with the common accents taken off, so "joao" finds "João".
fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// The candidates `query` names: any whose name contains it, or whose number
/// does. An empty query is everyone.
pub(super) fn matching(candidates: &[MentionCandidate], query: &str) -> Vec<MentionCandidate> {
    let query = fold(query.trim_end());
    let digits: String = query.chars().filter(char::is_ascii_digit).collect();
    candidates
        .iter()
        .filter(|candidate| {
            query.is_empty()
                || fold(&candidate.name).contains(&query)
                || (!digits.is_empty()
                    && digits.len() == query.chars().filter(|c| !c.is_whitespace()).count()
                    && candidate.user().contains(&digits))
        })
        .cloned()
        .collect()
}

/// The token a picked member becomes.
pub(super) fn token_for(candidate: &MentionCandidate) -> InlineToken {
    InlineToken::new(
        format!("{TOKEN_PREFIX}{}", candidate.jid),
        format!("@{}", candidate.user()),
    )
    .with_label(format!("@{}", candidate.name))
}

/// Whether a composer token is a mention, rather than an emoji.
pub(super) fn is_mention(token: &InlineToken) -> bool {
    token.id().starts_with(TOKEN_PREFIX)
}

/// What a send of `content` is made of: the text as the person reads it
/// (names), the text as the wire carries it (`@` plus user parts), and the
/// people it mentions, each once.
pub(super) fn split(content: &InputContent) -> (String, String, Vec<String>) {
    let wire = content.text().to_string();
    let mut display = String::with_capacity(wire.len());
    let mut jids: Vec<String> = Vec::new();
    let mut rest = 0;
    for span in content.tokens() {
        let token = span.token();
        if !is_mention(token) {
            continue;
        }
        let range = span.range();
        display.push_str(&wire[rest..range.start]);
        display.push_str(token.label());
        rest = range.end;
        let jid = token.id()[TOKEN_PREFIX.len()..].to_string();
        if !jids.contains(&jid) {
            jids.push(jid);
        }
    }
    display.push_str(&wire[rest..]);
    (display, wire, jids)
}

/// How a mention token draws in the field: its label, in the link colour,
/// the way a phone marks a mention while it is being written.
pub(super) fn render_token(
    label: &SharedString,
    line_height: gpui::Pixels,
    selected: bool,
    cx: &App,
) -> gpui::AnyElement {
    div()
        .h(line_height)
        .flex()
        .items_center()
        .text_color(cx.theme().link)
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(selected, |token| token.bg(cx.theme().selection))
        .child(label.clone())
        .into_any_element()
}

/// The list above the field while an `@` is being typed.
pub(super) fn render(
    query: &MentionQuery,
    entity: Entity<InputAreaView>,
    metrics: Metrics,
    cx: &App,
) -> gpui::AnyElement {
    let row_height = metrics.icon_button() * 1.4;
    // The window of rows that keeps the selected one in view.
    let first = query
        .selected
        .saturating_sub(VISIBLE - 1)
        .min(query.matches.len().saturating_sub(VISIBLE));
    div()
        .mb(metrics.space_sm())
        .flex()
        .flex_col()
        .py(metrics.space_xxs())
        .rounded(metrics.radius_md())
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().popover)
        .children(
            query
                .matches
                .iter()
                .enumerate()
                .skip(first)
                .take(VISIBLE)
                .map(|(ix, candidate)| {
                    let selected = ix == query.selected;
                    let entity = entity.clone();
                    let name: SharedString = candidate.name.clone().into();
                    let detail: Option<SharedString> = candidate.detail.clone().map(Into::into);
                    div()
                        .id(("mention-candidate", ix))
                        .h(row_height)
                        .px(metrics.space_lg())
                        .flex()
                        .items_center()
                        .gap(metrics.space_md())
                        .cursor_pointer()
                        .when(selected, |row| row.bg(cx.theme().accent))
                        .hover(|row| row.bg(cx.theme().accent))
                        .child(
                            div()
                                .min_w_0()
                                .text_size(metrics.text_small())
                                .text_color(cx.theme().popover_foreground)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(name),
                        )
                        .children(detail.map(|detail| {
                            div()
                                .flex_shrink_0()
                                .text_size(metrics.text_micro())
                                .text_color(cx.theme().muted_foreground)
                                .child(detail)
                        }))
                        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                            // Keep the field focused: the pick edits it.
                            window.prevent_default();
                        })
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |view, cx| view.pick_mention(ix, window, cx));
                        })
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn people() -> Vec<MentionCandidate> {
        vec![
            MentionCandidate {
                jid: "559900000001@s.whatsapp.net".into(),
                name: "Ana Souza".into(),
                detail: Some("+55 99 0000-0001".into()),
            },
            MentionCandidate {
                jid: "123456789@lid".into(),
                name: "João".into(),
                detail: None,
            },
        ]
    }

    #[test]
    fn an_at_starting_a_word_opens_the_list() {
        assert_eq!(query_at("oi @", 4, &[]), Some((3, String::new())));
        assert_eq!(query_at("@an", 3, &[]), Some((0, "an".into())));
        assert_eq!(query_at("oi (@jo", 7, &[]), Some((4, "jo".into())));
        assert_eq!(query_at("@ana sou", 8, &[]), Some((0, "ana sou".into())));
    }

    #[test]
    fn an_at_that_is_not_a_mention_does_not() {
        assert_eq!(query_at("mail@example", 12, &[]), None);
        assert_eq!(query_at("@ ana", 5, &[]), None);
        assert_eq!(query_at("@ana\nx", 6, &[]), None);
        assert_eq!(query_at("sem arroba", 10, &[]), None);
        // The `@` of a finished mention token.
        let token = std::ops::Range { start: 0, end: 13 };
        assert_eq!(query_at("@559900000001 ", 14, &[token]), None);
    }

    #[test]
    fn names_match_without_accents_and_numbers_by_digits() {
        let all = people();
        assert_eq!(matching(&all, "").len(), 2);
        assert_eq!(matching(&all, "joao")[0].name, "João");
        assert_eq!(matching(&all, "SOU")[0].name, "Ana Souza");
        assert_eq!(matching(&all, "0001")[0].name, "Ana Souza");
        assert!(matching(&all, "zé").is_empty());
    }

    #[test]
    fn a_draft_splits_into_what_is_read_and_what_is_sent() {
        let ana = &people()[0];
        let token = token_for(ana);
        let text = format!("oi {} e {}!", token.text(), token.text());
        let first = 3..3 + token.text().len();
        let second = first.end + 3..first.end + 3 + token.text().len();
        let content = InputContent::new(text.clone())
            .with_token(first, token.clone())
            .unwrap()
            .with_token(second, token)
            .unwrap();
        let (display, wire, jids) = split(&content);
        assert_eq!(display, "oi @Ana Souza e @Ana Souza!");
        assert_eq!(wire, "oi @559900000001 e @559900000001!");
        assert_eq!(jids, vec!["559900000001@s.whatsapp.net".to_string()]);
    }
}
