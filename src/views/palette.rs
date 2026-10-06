//! The cmd-K palette: jump to any loaded resource, or run a global command.
//!
//! It searches what the session has already cached, so opening it never
//! sends a request. Tables only appear for databases expanded at least once.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Action, App, Entity, Focusable as _, WeakEntity, Window, px};
use gpui_component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_component::{IndexPath, WindowExt as _};
use gpui_kit_assets::IconName as Lucide;

use crate::actions::{NewSqlTab, RefreshResources, SwitchAccount, ToggleDarkMode, ToggleSidebar};
use crate::state::Resource;
use crate::views::workspace::Workspace;

enum Entry {
    Resource(Resource),
    Command(Box<dyn Action>),
}

/// A group heading, its icon, and which resources belong under it.
type Group = (&'static str, Lucide, fn(&Resource) -> bool);

struct Section {
    label: &'static str,
    entries: Vec<(String, Lucide, Entry)>,
}

fn sections(resources: Vec<Resource>) -> Vec<Section> {
    let groups: [Group; 5] = [
        ("D1 databases", Lucide::Database, |r| {
            matches!(r, Resource::D1Database { .. })
        }),
        ("D1 tables", Lucide::Table, |r| {
            matches!(r, Resource::D1Table { .. })
        }),
        ("KV namespaces", Lucide::KeyRound, |r| {
            matches!(r, Resource::KvNamespace { .. })
        }),
        ("R2 buckets", Lucide::Archive, |r| {
            matches!(r, Resource::R2Bucket(_))
        }),
        ("Workers", Lucide::ScrollText, |r| {
            matches!(r, Resource::Worker { .. })
        }),
    ];
    let mut out: Vec<Section> = groups
        .iter()
        .map(|(label, icon, belongs)| Section {
            label,
            entries: resources
                .iter()
                .filter(|r| belongs(r))
                .map(|r| (r.label(), *icon, Entry::Resource(r.clone())))
                .collect(),
        })
        .filter(|s| !s.entries.is_empty())
        .collect();
    let command = |label: &str, icon, action: Box<dyn Action>| {
        (label.to_string(), icon, Entry::Command(action))
    };
    out.push(Section {
        label: "Commands",
        entries: vec![
            command(
                "Refresh resources",
                Lucide::RefreshCw,
                Box::new(RefreshResources),
            ),
            command("New SQL tab", Lucide::SquareTerminal, Box::new(NewSqlTab)),
            command("Toggle sidebar", Lucide::PanelLeft, Box::new(ToggleSidebar)),
            command("Toggle dark mode", Lucide::Moon, Box::new(ToggleDarkMode)),
            command("Switch account…", Lucide::Users, Box::new(SwitchAccount)),
        ],
    });
    out
}

pub fn open(
    workspace: WeakEntity<Workspace>,
    state: Entity<CommandState>,
    resources: Vec<Resource>,
    window: &mut Window,
    cx: &mut App,
) {
    state.update(cx, |s, cx| s.set_query("", window, cx));
    let sections = Rc::new(sections(resources));
    let focus_on_mount = Rc::new(Cell::new(true));
    window.open_dialog(cx, move |dialog, _, _| {
        let (state, sections, workspace, focus_on_mount) = (
            state.clone(),
            sections.clone(),
            workspace.clone(),
            focus_on_mount.clone(),
        );
        dialog
            .close_button(false)
            .p_0()
            .content(move |content, window, cx| {
                // The palette's input only exists once the dialog has rendered.
                if focus_on_mount.replace(false) {
                    let state = state.clone();
                    window.defer(cx, move |window, cx| {
                        state.read(cx).focus_handle(cx).focus(window, cx)
                    });
                }
                let command = sections.iter().fold(
                    Command::new(&state)
                        .bordered(false)
                        .placeholder("Jump to a database, table, namespace, bucket or Worker…")
                        .max_h(px(440.)),
                    |command, section| {
                        command.group(CommandGroup::new().label(section.label).items(
                            section.entries.iter().map(|(label, icon, entry)| {
                                let item = CommandItem::new().label(label.clone()).icon(*icon);
                                match entry {
                                    Entry::Resource(r) => item.keywords([r.kind()]),
                                    Entry::Command(_) => item,
                                }
                            }),
                        ))
                    },
                );
                let (sections, workspace) = (sections.clone(), workspace.clone());
                content.child(command.on_confirm(move |ix: IndexPath, window, cx| {
                    confirm(&sections, ix, &workspace, window, cx)
                }))
            })
    });
}

fn confirm(
    sections: &[Section],
    ix: IndexPath,
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some((_, _, entry)) = sections.get(ix.section).and_then(|s| s.entries.get(ix.row)) else {
        return;
    };
    window.close_dialog(cx);
    match entry {
        Entry::Resource(resource) => {
            let resource = resource.clone();
            let _ = workspace.update(cx, |w, cx| w.open(resource, window, cx));
        }
        Entry::Command(action) => {
            // Dispatched once the dialog is gone, so it starts from the
            // element that had focus before the palette opened.
            let action = action.boxed_clone();
            window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_resources_by_kind_and_skips_empty_groups() {
        let resources = vec![
            Resource::Worker { name: "api".into() },
            Resource::D1Database {
                id: "db".into(),
                name: "main".into(),
            },
            Resource::D1Table {
                database_id: "db".into(),
                database_name: "main".into(),
                table: "users".into(),
            },
        ];
        let sections = sections(resources);
        let labels: Vec<_> = sections.iter().map(|s| s.label).collect();
        assert_eq!(labels, ["D1 databases", "D1 tables", "Workers", "Commands"]);
        assert_eq!(sections[1].entries[0].0, "main.users");
    }
}
