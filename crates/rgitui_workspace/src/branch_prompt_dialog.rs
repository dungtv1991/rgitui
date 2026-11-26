//! One modal for the branch context-menu actions that need input: editing a
//! branch description, picking a tracking branch, and picking a ref to compare
//! a branch with.
use gpui::prelude::*;
use gpui::{
    div, px, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle, Render,
    SharedString, Window,
};
use rgitui_i18n::{key, t};
use rgitui_theme::{ActiveTheme, Color, StyledExt};
use rgitui_ui::{
    Button, ButtonSize, ButtonStyle, Icon, IconName, IconSize, Label, LabelSize, TextInput,
    TextInputEvent,
};

use crate::keymap;
use crate::CommandId;

/// Most candidate rows shown at once.
const MAX_ROWS: usize = 10;

/// What the dialog is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchPromptKind {
    Description,
    TrackingBranch,
    CompareWith,
}

/// Events emitted by the dialog.
#[derive(Debug, Clone)]
pub enum BranchPromptEvent {
    Description { branch: String, text: String },
    TrackingBranch { branch: String, upstream: String },
    CompareWith { branch: String, other: String },
    Dismissed,
}

/// Case-insensitive filter over `candidates`, returning indices of the matches.
fn filter_candidates(candidates: &[String], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| query.is_empty() || c.to_lowercase().contains(&query))
        .map(|(i, _)| i)
        .collect()
}

pub struct BranchPromptDialog {
    kind: BranchPromptKind,
    branch: String,
    candidates: Vec<String>,
    editor: Entity<TextInput>,
    visible: bool,
    pending_focus: bool,
    focus_handle: FocusHandle,
}

impl EventEmitter<BranchPromptEvent> for BranchPromptDialog {}

impl BranchPromptDialog {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let editor = cx.new(TextInput::new);
        cx.subscribe(
            &editor,
            |this: &mut Self, _, event: &TextInputEvent, cx| match event {
                TextInputEvent::Submit => this.submit_first(cx),
                TextInputEvent::Blurred => {}
                TextInputEvent::Changed(_) => cx.notify(),
            },
        )
        .detach();
        Self {
            kind: BranchPromptKind::Description,
            branch: String::new(),
            candidates: Vec::new(),
            editor,
            visible: false,
            pending_focus: false,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Ask for a new description of `branch`, starting from `current`.
    pub fn show_description(&mut self, branch: String, current: String, cx: &mut Context<Self>) {
        self.kind = BranchPromptKind::Description;
        self.branch = branch;
        self.candidates.clear();
        self.editor.update(cx, |e, cx| {
            e.set_placeholder_message(key!("branch_prompt.description_placeholder"));
            e.set_text(current, cx);
        });
        self.open(cx);
    }

    /// Ask the user to pick one of `candidates` for `branch`.
    pub fn show_picker(
        &mut self,
        kind: BranchPromptKind,
        branch: String,
        candidates: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.kind = kind;
        self.branch = branch;
        self.candidates = candidates;
        self.editor.update(cx, |e, cx| {
            e.set_placeholder_message(key!("branch_prompt.filter_placeholder"));
            e.clear(cx);
        });
        self.open(cx);
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        self.visible = true;
        self.pending_focus = true;
        cx.notify();
    }

    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.close(cx);
        cx.emit(BranchPromptEvent::Dismissed);
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.visible = false;
        self.candidates.clear();
        self.editor.update(cx, |e, cx| e.clear(cx));
        cx.notify();
    }

    /// Enter in the field: save the text, or pick the first match.
    fn submit_first(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        if self.kind == BranchPromptKind::Description {
            let text = self.editor.read(cx).text().to_string();
            let branch = std::mem::take(&mut self.branch);
            self.close(cx);
            cx.emit(BranchPromptEvent::Description { branch, text });
            return;
        }
        let query = self.editor.read(cx).text().to_string();
        if let Some(&index) = filter_candidates(&self.candidates, &query).first() {
            self.pick(index, cx);
        }
    }

    fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(chosen) = self.candidates.get(index).cloned() else {
            return;
        };
        let branch = std::mem::take(&mut self.branch);
        let kind = self.kind;
        self.close(cx);
        match kind {
            BranchPromptKind::TrackingBranch => cx.emit(BranchPromptEvent::TrackingBranch {
                branch,
                upstream: chosen,
            }),
            BranchPromptKind::CompareWith => cx.emit(BranchPromptEvent::CompareWith {
                branch,
                other: chosen,
            }),
            BranchPromptKind::Description => {}
        }
    }

    fn dispatch_command(&mut self, cmd: CommandId, _window: &mut Window, cx: &mut Context<Self>) {
        match cmd {
            CommandId::Cancel => self.dismiss(cx),
            _ => cx.propagate(),
        }
    }
}

impl Render for BranchPromptDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().id("branch-prompt-dialog").into_any_element();
        }
        if self.pending_focus {
            self.pending_focus = false;
            self.editor.update(cx, |e, cx| e.focus(window, cx));
        }
        let colors = cx.colors().clone();
        let accent = Color::Accent.color(cx);
        let icon_bg = gpui::Hsla { a: 0.12, ..accent };
        let is_description = self.kind == BranchPromptKind::Description;
        let (title, icon) = match self.kind {
            BranchPromptKind::Description => {
                (t!("branch_prompt.description_title"), IconName::Edit)
            }
            BranchPromptKind::TrackingBranch => {
                (t!("branch_prompt.tracking_title"), IconName::GitBranch)
            }
            BranchPromptKind::CompareWith => {
                (t!("branch_prompt.compare_title"), IconName::GitCompare)
            }
        };
        let branch_name: SharedString = self.branch.clone().into();

        let mut modal = div()
            .id("branch-prompt-dialog-modal")
            .track_focus(&self.focus_handle)
            .map(|el| {
                keymap::bind_actions(
                    el,
                    "BranchPromptDialog",
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
            });

        modal = modal.child(
            div()
                .h_flex()
                .gap_3()
                .items_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(36.))
                        .rounded(px(10.))
                        .bg(icon_bg)
                        .child(Icon::new(icon).size(IconSize::Medium).color(Color::Accent)),
                )
                .child(
                    Label::new(title)
                        .size(LabelSize::Large)
                        .weight(gpui::FontWeight::BOLD),
                ),
        );

        modal = modal.child(
            div()
                .h_flex()
                .gap(px(8.))
                .items_center()
                .child(
                    Label::new(t!("branch_prompt.branch"))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .child(
                    div()
                        .h_flex()
                        .h(px(22.))
                        .px(px(8.))
                        .gap(px(4.))
                        .rounded(px(6.))
                        .bg(colors.ghost_element_selected)
                        .items_center()
                        .child(
                            Icon::new(IconName::GitBranch)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                        .child(
                            Label::new(branch_name)
                                .size(LabelSize::XSmall)
                                .weight(gpui::FontWeight::MEDIUM)
                                .color(Color::Muted),
                        ),
                ),
        );

        modal = modal.child(div().w_full().child(self.editor.clone()));

        if !is_description {
            let query = self.editor.read(cx).text().to_string();
            let matches = filter_candidates(&self.candidates, &query);
            let mut list = div().v_flex().w_full().gap(px(2.));
            if matches.is_empty() {
                list = list.child(
                    Label::new(t!("branch_prompt.no_matches"))
                        .size(LabelSize::XSmall)
                        .color(Color::Placeholder),
                );
            }
            for (row, &index) in matches.iter().take(MAX_ROWS).enumerate() {
                let name: SharedString = self.candidates[index].clone().into();
                list = list.child(
                    div()
                        .id(ElementId::NamedInteger(
                            "branch-prompt-row".into(),
                            index as u64,
                        ))
                        .h_flex()
                        .w_full()
                        .h(px(26.))
                        .px(px(8.))
                        .gap(px(6.))
                        .items_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(row == 0, |el| el.bg(colors.ghost_element_selected))
                        .hover(|s| s.bg(colors.ghost_element_hover))
                        .active(|s| s.bg(colors.ghost_element_active))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.pick(index, cx);
                        }))
                        .child(
                            Icon::new(IconName::GitBranch)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                        .child(Label::new(name).size(LabelSize::Small).truncate()),
                );
            }
            modal = modal.child(list);
        }

        let hint = if is_description {
            t!("branch_prompt.hint_save")
        } else {
            t!("branch_prompt.hint_pick")
        };
        let mut footer_buttons = div().h_flex().gap_2().child(
            Button::new("cancel-branch-prompt", t!("common.cancel"))
                .size(ButtonSize::Default)
                .style(ButtonStyle::Subtle)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.dismiss(cx);
                })),
        );
        if is_description {
            footer_buttons = footer_buttons.child(
                Button::new("save-branch-prompt", t!("branch_prompt.save"))
                    .icon(IconName::Check)
                    .size(ButtonSize::Default)
                    .style(ButtonStyle::Filled)
                    .color(Color::Accent)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.submit_first(cx);
                    })),
            );
        }
        modal = modal.child(
            div()
                .pt_2()
                .border_t_1()
                .border_color(colors.border_variant)
                .h_flex()
                .justify_between()
                .items_center()
                .child(
                    Label::new(hint)
                        .size(LabelSize::XSmall)
                        .color(Color::Placeholder),
                )
                .child(footer_buttons),
        );

        div()
            .id("branch-prompt-dialog-backdrop")
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
    use super::filter_candidates;

    fn names() -> Vec<String> {
        ["origin/main", "origin/dev", "upstream/Main"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(filter_candidates(&names(), "  "), vec![0, 1, 2]);
    }

    #[test]
    fn filter_ignores_case() {
        assert_eq!(filter_candidates(&names(), "MAIN"), vec![0, 2]);
    }

    #[test]
    fn no_match_is_empty() {
        assert!(filter_candidates(&names(), "zzz").is_empty());
    }
}
