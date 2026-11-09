//! Draws a model-layer [`Notice`] (#301): the element id and `aria_label`
//! tests find it by, and the theme colour of its [`Intent`].

use astrolabe_model::notice::{Intent, Notice};
use gpui_kit::base::test_support::Observed;
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::*;

/// The theme colour `intent` is drawn in.
pub(crate) fn intent_color(intent: Intent, theme: &Theme) -> Hsla {
    match intent {
        Intent::Neutral => theme.foreground,
        Intent::Muted => theme.muted_foreground,
        Intent::Success => theme.success,
        Intent::Warning => theme.warning,
        Intent::Danger => theme.danger,
    }
}

/// The plain text of `notice`; the caller sizes and pads it. Banners
/// (`Alert`, which carry `Role::Alert`) are not drawn here; the Logs and Exec
/// status bars are (`terminal_panel`).
pub(crate) fn notice_view(notice: Notice, cx: &App) -> Observed<Stateful<Div>> {
    let color = intent_color(notice.intent, cx.theme());
    div()
        .id(notice.id)
        .aria_label(notice.text.clone())
        .test_support()
        .text_color(color)
        .child(notice.text)
}

#[cfg(test)]
mod tests {
    use super::intent_color;
    use astrolabe_model::notice::Intent;
    use gpui_kit::component::{Theme, ThemeColor};
    use gpui_kit::hsla;

    /// A theme whose five intent colours differ, so a swapped arm shows.
    fn distinct_theme() -> Theme {
        Theme {
            colors: ThemeColor {
                foreground: hsla(0.0, 0.0, 0.1, 1.0),
                muted_foreground: hsla(0.1, 0.0, 0.5, 1.0),
                success: hsla(0.3, 1.0, 0.4, 1.0),
                warning: hsla(0.15, 1.0, 0.5, 1.0),
                danger: hsla(0.0, 1.0, 0.5, 1.0),
                ..ThemeColor::default()
            },
            ..Theme::default()
        }
    }

    #[test]
    fn each_intent_is_drawn_in_its_own_theme_colour() {
        let theme = distinct_theme();
        assert_eq!(intent_color(Intent::Neutral, &theme), theme.foreground);
        assert_eq!(intent_color(Intent::Muted, &theme), theme.muted_foreground);
        assert_eq!(intent_color(Intent::Success, &theme), theme.success);
        assert_eq!(intent_color(Intent::Warning, &theme), theme.warning);
        assert_eq!(intent_color(Intent::Danger, &theme), theme.danger);
    }

    #[test]
    fn a_failed_detail_and_related_load_is_drawn_in_the_danger_colour() {
        use astrolabe_model::detail::{DetailOutcome, RelatedOutcome};
        let theme = distinct_theme();
        let detail = DetailOutcome::Failed(None).notice("Pod web").unwrap();
        let related = RelatedOutcome::Failed(None).notice().unwrap();
        for notice in [detail, related] {
            assert_eq!(intent_color(notice.intent, &theme), theme.danger);
            assert_ne!(intent_color(notice.intent, &theme), theme.muted_foreground);
        }
    }
}
