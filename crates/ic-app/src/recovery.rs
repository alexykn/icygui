//! The screen shown instead of the workspace when the settings file can't
//! be read (OPS-06): what is wrong, and the choices: restore the backup,
//! start fresh, try again, or quit. The unreadable file is never replaced
//! without asking, and starting fresh keeps it as a copy.

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use ic_ui_kit::{ActiveTheme as _, Button, CodeBlock, EmptyState, Icon, IconName, PaneHeader};

use crate::app_state::ConfigProblem;
use crate::chrome::{Controls, WindowControls, WindowDrag};
use crate::live::{self, RecoveryChoice};

/// The texts of the recovery screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryText {
    /// The headline.
    pub(crate) title: String,
    /// What happened, in a sentence.
    pub(crate) explanation: String,
    /// What the backup offers: restorable, absent, or unreadable too.
    pub(crate) backup: String,
    /// Whether "Restore backup" is offered.
    pub(crate) can_restore: bool,
}

/// What the screen says about `problem`.
pub(crate) fn text(problem: &ConfigProblem) -> RecoveryText {
    let file = problem.path.display();
    let (title, explanation) = if problem.newer {
        (
            "These settings are from a newer icygui".to_owned(),
            format!(
                "{file} was written by a newer version of icygui. Update icygui to keep them, \
                 or start fresh here: the file is kept as a copy next to it."
            ),
        )
    } else {
        (
            "icygui can't read its settings".to_owned(),
            format!(
                "{file} can't be read. Fix it by hand and try again, restore the backup from \
                 before the last change, or start fresh: the file is kept as a copy next to it."
            ),
        )
    };
    let (backup, can_restore) = match &problem.backup {
        Ok(Some(config)) => (
            format!(
                "The backup has {} environment{}.",
                config.environments.len(),
                if config.environments.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            true,
        ),
        Ok(None) => ("There is no backup.".to_owned(), false),
        Err(error) => (format!("The backup can't be read either: {error}"), false),
    };
    RecoveryText {
        title,
        explanation,
        backup,
        can_restore,
    }
}

/// The recovery screen, filling the window.
pub(crate) fn render<T: 'static>(
    problem: &ConfigProblem,
    drag: &WindowDrag,
    window: &Window,
    cx: &Context<T>,
) -> AnyElement {
    let theme = cx.theme();
    let text = text(problem);
    let controls = Controls::of(window, cx);
    let mut header = PaneHeader::new("recovery-header")
        .padding(theme.metrics.sidebar_padding)
        .title("icygui");
    if controls != Controls::None {
        header = header.leading(WindowControls::new(controls));
    }
    let header = drag.attach(div().id("recovery-header-drag").child(header), controls);
    let busy = problem.busy;
    let choose = |choice: RecoveryChoice| {
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            if let Some(session) = live::session(cx) {
                session.update(cx, |session, cx| session.resolve_config(choice, cx));
            }
        }
    };
    let buttons = div()
        .flex()
        .flex_wrap()
        .justify_center()
        .gap(px(8.))
        .when(text.can_restore, |row| {
            row.child(
                Button::new("restore-backup", "restore backup")
                    .primary()
                    .disabled(busy)
                    .on_click(choose(RecoveryChoice::RestoreBackup)),
            )
        })
        .child(
            Button::new("start-fresh", "start fresh")
                .disabled(busy)
                .on_click(choose(RecoveryChoice::StartFresh)),
        )
        .child(
            Button::new("try-again", "try again")
                .disabled(busy)
                .on_click(choose(RecoveryChoice::Retry)),
        )
        .child(Button::new("quit", "quit").on_click(|_, _, cx| cx.quit()));
    let body = EmptyState::new(text.title)
        .leading(
            Icon::new(IconName::TriangleAlert)
                .size(px(22.))
                .color(theme.states.critical),
        )
        .detail(text.explanation)
        .max_width(px(640.))
        .child(
            div()
                .w(px(600.))
                .text_left()
                .child(CodeBlock::new(problem.message.clone())),
        )
        .child(
            div()
                .text_size(theme.text.small)
                .text_color(theme.colors.text_muted)
                .child(text.backup),
        )
        .child(buttons)
        .when(busy, |state| {
            state.child(
                div()
                    .text_size(theme.text.small)
                    .text_color(theme.colors.text_muted)
                    .child("Working"),
            )
        })
        .when_some(problem.failure.clone(), |state, failure| {
            state.child(
                div()
                    .max_w(px(600.))
                    .text_size(theme.text.small)
                    .text_color(theme.states.critical)
                    .child(failure),
            )
        });
    div()
        .id("recovery")
        .flex()
        .flex_col()
        .size_full()
        .bg(theme.colors.window_background)
        .child(header)
        .child(div().flex().flex_1().min_h_0().child(body))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ic_config::{AuthConfig, Config, Environment};

    use super::*;

    fn problem(backup: Result<Option<Config>, String>, newer: bool) -> ConfigProblem {
        ConfigProblem {
            message: "TOML parse error at line 3, column 7".to_owned(),
            path: PathBuf::from("/home/u/.config/icygui/config.toml"),
            backup,
            newer,
            busy: false,
            failure: None,
        }
    }

    #[test]
    fn a_readable_backup_can_be_restored() {
        let backup = Config {
            environments: vec![Environment::new(
                "prod",
                "https://m:5665",
                AuthConfig::Basic {
                    username: "u".to_owned(),
                },
            )],
            ..Config::default()
        };
        let text = text(&problem(Ok(Some(backup)), false));
        assert!(text.can_restore);
        assert_eq!(text.backup, "The backup has 1 environment.");
        assert_eq!(text.title, "icygui can't read its settings");
        assert!(
            text.explanation
                .contains("/home/u/.config/icygui/config.toml")
        );
    }

    #[test]
    fn without_a_usable_backup_only_fresh_and_retry_remain() {
        let none = text(&problem(Ok(None), false));
        assert!(!none.can_restore);
        assert_eq!(none.backup, "There is no backup.");
        let broken = text(&problem(Err("line 1".to_owned()), false));
        assert!(!broken.can_restore);
        assert!(broken.backup.contains("line 1"));
    }

    #[test]
    fn newer_files_say_so() {
        let text = text(&problem(Ok(None), true));
        assert_eq!(text.title, "These settings are from a newer icygui");
        assert!(text.explanation.contains("Update icygui"));
    }
}
