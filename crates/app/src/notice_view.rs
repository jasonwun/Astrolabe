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
/// (`Alert`, which carry `Role::Alert`) and status bars are not drawn here.
pub(crate) fn notice_view(notice: Notice, cx: &App) -> Observed<Stateful<Div>> {
    let color = intent_color(notice.intent, cx.theme());
    div()
        .id(notice.id)
        .aria_label(notice.text.clone())
        .test_support()
        .text_color(color)
        .child(notice.text)
}
