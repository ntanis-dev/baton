//! A dialog longer than its room: where it scrolls to, and what a click and the wheel land on.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use baton_core::theme::GlyphSet;
use baton_testkit::temp_home;
use baton_tui::keys::Key;
use baton_tui::layout::Size;
use baton_tui::mouse::{Hit, Target, hit_test};
use baton_tui::reducer::reduce;
use baton_tui::state::{Action, ChoiceOption, Choose, Effect, Modal, ModalKind, MouseEvent, State};
use baton_tui::ui::kit::theme_of;
use baton_tui::ui::overlays::modal_layout;

use common::{build_home, frame, menu_state_with};

fn long_choice(base: &State, index: usize, scroll: Option<usize>) -> State {
    let options = (1..=40)
        .map(|i| ChoiceOption {
            label: format!("Option number {i}"),
            value: format!("v{i}"),
        })
        .collect();
    let mut state = base.clone();
    state.modal = Some(Modal {
        title: "Pick one of many".to_owned(),
        lines: vec!["A line of text above the options.".to_owned()],
        scroll,
        kind: ModalKind::Choice {
            options,
            index,
            choose: Choose::HopTo {
                session: "s".to_owned(),
            },
            submit_label: None,
        },
    });
    state
}

fn choice(x: usize, y: usize, index: usize) -> Target {
    Target {
        x,
        y,
        width: 66,
        hit: Hit::Choice(index),
    }
}

#[test]
fn a_long_dialog_keeps_the_option_under_the_keyboard_in_view_and_the_wheel_moves_it() {
    let home = temp_home();
    build_home(&home.paths, false);
    let base = menu_state_with(&home.paths, Size { cols: 100, rows: 28 }, &[], GlyphSet::Rich);
    // (the option under the keyboard, where the dialog was scrolled to, the first line shown, the click
    // targets, the first target, the options at two rows, the options the wheel moves the keyboard to by
    // +3, -3 and +100, the option under the keyboard after the wheel's +3 and Down)
    let cases = [
        (0, None, 0, 20, choice(17, 4, 0), [None, Some(6)], [3, 0, 39], 4),
        (
            20,
            None,
            13,
            23,
            choice(17, 1, 10),
            [Some(11), Some(19)],
            [23, 17, 39],
            24,
        ),
        (
            39,
            None,
            22,
            23,
            choice(17, 1, 19),
            [Some(20), Some(28)],
            [39, 36, 39],
            0,
        ),
        (
            5,
            Some(12),
            12,
            23,
            choice(17, 1, 9),
            [Some(10), Some(18)],
            [8, 2, 39],
            9,
        ),
    ];
    for (index, scroll, offset, targets, first, at_rows, wheel, after) in cases {
        let state = long_choice(&base, index, scroll);
        let layout = modal_layout(&state, &theme_of(&state)).unwrap();
        assert_eq!(
            (
                layout.top,
                layout.left,
                layout.width,
                layout.offset,
                layout.max,
                layout.room,
                layout.lines.len()
            ),
            (0, 15, 70, offset, 22, 21, 25),
            "option {index}"
        );
        assert_eq!(layout.targets.len(), targets, "option {index}");
        assert_eq!(layout.targets[0], first, "option {index}");
        let cancel = layout.targets.last().unwrap();
        assert_eq!(
            (cancel.x, cancel.y, cancel.width, &cancel.hit),
            (40, 23, 13, &Hit::Command(Key::Escape))
        );
        let click = |x: i32, y: i32| hit_test(&state, &MouseEvent::press(x, y));
        assert_eq!(click(20, 2), at_rows[0].map(Hit::Choice), "option {index}");
        assert_eq!(click(20, 10), at_rows[1].map(Hit::Choice), "option {index}");
        assert_eq!(click(60, 23), None);
        assert_eq!(click(18, 23), Some(Hit::Command(Key::Enter)));
        // Beside the dialog.
        assert_eq!(click(5, 10), Some(Hit::Outside));
        for (delta, option) in [3, -3, 100].into_iter().zip(wheel) {
            assert_eq!(
                hit_test(&state, &MouseEvent::wheel(50, 10, delta)),
                Some(Hit::ModalIndex(option)),
                "option {index}, wheel {delta}"
            );
        }
        // The wheel moves the option under the keyboard, never past the ends, and the dialog follows
        // it; so does the next arrow key.
        let wheeled = reduce(&state, Action::Mouse(MouseEvent::wheel(50, 10, 3))).state;
        let at = wheeled.modal.as_ref().unwrap();
        assert_eq!((at.index(), at.scroll), (Some(wheel[0]), None), "option {index}");
        let moved = reduce(&wheeled, Action::Key(Key::Down)).state.modal.unwrap();
        assert_eq!(
            (moved.index(), moved.scroll),
            (Some(after), None),
            "option {index}"
        );
    }
    // A dialog without options scrolls with the wheel, from its end up.
    let mut long = base.clone();
    long.modal = Some(Modal {
        title: "A long question".to_owned(),
        lines: (1..=40).map(|i| format!("Line {i}")).collect(),
        scroll: None,
        kind: ModalKind::Confirm {
            yes: false,
            focus: None,
            effect: Effect::Quit,
        },
    });
    let layout = modal_layout(&long, &theme_of(&long)).unwrap();
    assert!(layout.max > 3 && layout.offset == layout.max);
    assert_eq!(
        hit_test(&long, &MouseEvent::wheel(50, 10, -3)),
        Some(Hit::ModalScroll(layout.max - 3))
    );
    assert_eq!(
        hit_test(&long, &MouseEvent::wheel(50, 10, 3)),
        Some(Hit::ModalScroll(layout.max))
    );
    let scrolled = reduce(&long, Action::Mouse(MouseEvent::wheel(50, 10, -3))).state;
    assert_eq!(scrolled.modal.unwrap().scroll, Some(layout.max - 3));
    // The dialog floats over the sidebar and the pane; what is beside it still shows.
    let rows = frame(&long_choice(&base, 20, None));
    assert_eq!(
        rows[..2],
        [
            "               ╭─ Pick one of many ─────────────────────────────────────────────────╮",
            "       ____  __│    11 Option number 11                                             │",
        ]
    );
    // After the dialog's right edge: the last two cells of the pane's note (the minutes of a time).
    let beside = "      / __ )/  │    12 Option number 12                                             │";
    assert!(
        rows[2].starts_with(beside) && rows[2].chars().count() == beside.chars().count() + 2,
        "{}",
        rows[2]
    );
    assert_eq!(
        rows[3],
        "     / __  / /|│    13 Option number 13                                             │"
    );
}
