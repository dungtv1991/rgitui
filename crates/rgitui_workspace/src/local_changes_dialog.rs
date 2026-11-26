//! Asks what to do with uncommitted changes before an operation that needs a
//! clean working tree (checkout, rebase).
use gpui::prelude::*;
use gpui::{div, px, ClickEvent, Context, ElementId, EventEmitter, FocusHandle, Render, Window};
use rgitui_i18n::t;
use rgitui_theme::{ActiveTheme, Color, StyledExt};
use rgitui_ui::{Button, ButtonSize, ButtonStyle, Icon, IconName, IconSize, Label, LabelSize};

use crate::keymap;
use crate::CommandId;

/// The operation waiting on the user's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalChangesAction {
    /// Check out the named branch.
    Checkout(String),
    /// Rebase the checked-out branch onto the named branch.
    Rebase(String),
}

impl LocalChangesAction {
    /// The branch the operation targets.
    pub fn branch(&self) -> &str {
        match self {
            LocalChangesAction::Checkout(name) | LocalChangesAction::Rebase(name) => name,
        }
    }
}

/// What to do with uncommitted changes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalChangesChoice {
    /// Run the operation as is. Git refuses if the changes are in the way.
    #[default]
    DoNothing,
    /// Stash the changes, run the operation, then pop the stash.
    StashAndReapply,
    /// Throw the changes away.
    Discard,
}

impl LocalChangesChoice {
    const ALL: [LocalChangesChoice; 3] = [
        LocalChangesChoice::DoNothing,
        LocalChangesChoice::StashAndReapply,
        LocalChangesChoice::Discard,
    ];
}

/// Events emitted by the dialog.
#[derive(Debug, Clone)]
pub enum LocalChangesDialogEvent {
    Confirmed {
        action: LocalChangesAction,
        choice: LocalChangesChoice,
    },
    Dismissed,
}

pub struct LocalChangesDialog {
    action: Option<LocalChangesAction>,
    choice: LocalChangesChoice,
    visible: bool,
    pending_focus: bool,
    focus_handle: FocusHandle,
}

impl EventEmitter<LocalChangesDialogEvent> for LocalChangesDialog {}

impl LocalChangesDialog {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            action: None,
            choice: LocalChangesChoice::default(),
            visible: false,
            pending_focus: false,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Ask what to do with local changes before `action` runs.
    pub fn show_visible(&mut self, action: LocalChangesAction, cx: &mut Context<Self>) {
        self.action = Some(action);
        self.choice = LocalChangesChoice::default();
        self.visible = true;
        self.pending_focus = true;
        cx.notify();
    }

    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.visible = false;
        self.action = None;
        cx.emit(LocalChangesDialogEvent::Dismissed);
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        let Some(action) = self.action.take() else {
            return;
        };
        self.visible = false;
        cx.emit(LocalChangesDialogEvent::Confirmed {
            action,
            choice: self.choice,
        });
        cx.notify();
    }

    fn dispatch_command(&mut self, cmd: CommandId, _window: &mut Window, cx: &mut Context<Self>) {
        match cmd {
            CommandId::Cancel => self.dismiss(cx),
            CommandId::Confirm => self.confirm(cx),
            _ => cx.propagate(),
        }
    }
}

impl Render for LocalChangesDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(action) = self.action.clone().filter(|_| self.visible) else {
            return div().id("local-changes-dialog").into_any_element();
        };
        if self.pending_focus {
            self.pending_focus = false;
            self.focus_handle.focus(window, cx);
        }
        let colors = cx.colors().clone();
        let (title, icon) = match action {
            LocalChangesAction::Checkout(_) => {
                (t!("local_changes.checkout_title"), IconName::Check)
            }
            LocalChangesAction::Rebase(_) => {
                (t!("local_changes.rebase_title"), IconName::GitBranch)
            }
        };
        let label_width = px(110.);

        let mut options = div().v_flex().gap(px(8.));
        for (index, option) in LocalChangesChoice::ALL.into_iter().enumerate() {
            let selected = self.choice == option;
            let text = match option {
                LocalChangesChoice::DoNothing => t!("local_changes.do_nothing"),
                LocalChangesChoice::StashAndReapply => t!("local_changes.stash_and_reapply"),
                LocalChangesChoice::Discard => t!("local_changes.discard"),
            };
            options = options.child(
                div()
                    .id(ElementId::NamedInteger(
                        "local-changes-option".into(),
                        index as u64,
                    ))
                    .h_flex()
                    .gap(px(8.))
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.choice = option;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(16.))
                            .rounded_full()
                            .border_2()
                            .border_color(if selected {
                                colors.text_accent
                            } else {
                                colors.border
                            })
                            .when(selected, |el| {
                                el.child(div().size(px(8.)).rounded_full().bg(colors.text_accent))
                            }),
                    )
                    .child(Label::new(text).size(LabelSize::Small)),
            );
        }

        let modal = div()
            .id("local-changes-dialog-modal")
            .track_focus(&self.focus_handle)
            .map(|el| {
                keymap::bind_actions(
                    el,
                    "LocalChangesDialog",
                    &["Menu"],
                    cx,
                    Self::dispatch_command,
                )
            })
            .v_flex()
            .w(px(440.))
            .elevation_3(cx)
            .p(px(20.))
            .gap(px(16.))
            .on_click(|_: &ClickEvent, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(icon).size(IconSize::Medium).color(Color::Default))
                    .child(
                        Label::new(title)
                            .size(LabelSize::Large)
                            .weight(gpui::FontWeight::BOLD),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap(px(8.))
                    .items_center()
                    .child(
                        div().w(label_width).flex().justify_end().child(
                            Label::new(t!("local_changes.branch"))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                    )
                    .child(
                        Icon::new(IconName::GitBranch)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .child(Label::new(action.branch().to_string()).size(LabelSize::Small)),
            )
            .child(
                div()
                    .h_flex()
                    .gap(px(8.))
                    .items_start()
                    .child(
                        div().w(label_width).flex().justify_end().child(
                            Label::new(t!("local_changes.local_changes"))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                    )
                    .child(options),
            )
            .child(
                div()
                    .h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("confirm-local-changes", t!("local_changes.ok"))
                            .size(ButtonSize::Default)
                            .style(ButtonStyle::Filled)
                            .color(Color::Accent)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.confirm(cx);
                            })),
                    )
                    .child(
                        Button::new("cancel-local-changes", t!("common.cancel"))
                            .size(ButtonSize::Default)
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.dismiss(cx);
                            })),
                    ),
            );

        div()
            .id("local-changes-dialog-backdrop")
            .occlude()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.0,
                a: 0.5,
            })
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.dismiss(cx);
            }))
            .child(modal)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn do_nothing_is_the_default_choice() {
        assert_eq!(LocalChangesChoice::default(), LocalChangesChoice::DoNothing);
    }

    #[test]
    fn action_exposes_its_branch() {
        assert_eq!(LocalChangesAction::Checkout("main".into()).branch(), "main");
        assert_eq!(LocalChangesAction::Rebase("dev".into()).branch(), "dev");
    }
}
