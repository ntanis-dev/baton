//! Every frame of the menu on the fixtures, as plain text: every screen at every size and the
//! all-blocked Overview; the screens the keys alone do not reach (`x-*`: the splash, the sign-in
//! screen, toasts, the plain glyphs); a home with long, wide and e-mail names (`y-*`) and one with no
//! account (`z-*`). The `frames` example writes them out for review, and `tests/golden.rs` holds them
//! against the ones on record.

use baton_core::lines::plain;
use baton_core::theme::GlyphSet;
use baton_testkit::{temp_home, write_json};
use baton_tui::layout::Size;
use baton_tui::state::{Action, Init, State, Tone, initial_state, toast};
use baton_tui::ui::login::login_frame;
use serde_json::json;

use super::{Input, build_home, frame, menu_state_with, press, screen, screen_state, screens, sizes, text};

/// A sign-in screen as text rows.
fn login(state: &State, url: Option<&str>, code_prompt: bool) -> Vec<String> {
    login_frame(state, "work@example.com", url, code_prompt)
        .rows
        .iter()
        .map(|row| plain(row).trim_end().to_owned())
        .collect()
}

/// A frame's rows as the text of its file: a line each.
pub fn as_text(rows: &[String]) -> String {
    rows.join("\n") + "\n"
}

/// Every frame, by its name.
pub fn every_frame() -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let normal = temp_home();
    let blocked = temp_home();
    build_home(&normal.paths, false);
    build_home(&blocked.paths, true);
    for (size_name, _) in sizes() {
        for (screen_name, _) in screens() {
            out.push((
                format!("{size_name}-{screen_name}"),
                frame(&screen_state(&normal.paths, size_name, screen_name)),
            ));
        }
        out.push((
            format!("{size_name}-overview-all-blocked"),
            frame(&screen_state(&blocked.paths, size_name, "overview")),
        ));
    }
    // What the keys alone do not reach.
    let paths = &normal.paths;
    let big = Size { cols: 120, rows: 30 };
    let base = menu_state_with(paths, big, &[], GlyphSet::Rich);
    let mut extra: Vec<(String, Vec<String>)> = Vec::new();
    for (name, size) in [
        ("120x30", big),
        ("100x28", Size { cols: 100, rows: 28 }),
        ("160x45", Size { cols: 160, rows: 45 }),
        ("101x29", Size { cols: 101, rows: 29 }),
        ("80x24", Size { cols: 80, rows: 24 }),
        ("60x10", Size { cols: 60, rows: 10 }),
        ("99x40", Size { cols: 99, rows: 40 }),
        ("40x6", Size { cols: 40, rows: 6 }),
    ] {
        let mut state = menu_state_with(paths, size, &[], GlyphSet::Rich);
        state.splash = true;
        extra.push((format!("splash-{name}"), frame(&state)));
    }
    for (name, size) in [
        ("100x28", Size { cols: 100, rows: 28 }),
        ("120x30", big),
        ("160x45", Size { cols: 160, rows: 45 }),
    ] {
        let state = initial_state(Init {
            size,
            color: false,
            ..Init::default()
        });
        let url = "https://claude.ai/oauth/authorize?client_id=fixture&state=public&code=true";
        extra.push((format!("login-{name}-code"), login(&state, Some(url), true)));
        extra.push((format!("login-{name}-waiting"), login(&state, None, false)));
        extra.push((format!("login-{name}-link"), login(&state, Some(url), false)));
    }
    for name in [
        "overview",
        "account",
        "stats",
        "session",
        "policy",
        "actions",
        "thresholds",
    ] {
        let state = menu_state_with(paths, big, &screen(name), GlyphSet::Plain);
        extra.push((format!("plain-{name}"), frame(&state)));
    }
    let mut toasts = toast(&base, "Saved the thresholds of acct2.", Tone::Ok);
    toasts = toast(&toasts, "acct3 is the name of another account.", Tone::Error);
    toasts = toast(
        &toasts,
        "A long note that has to wrap over more than one line of the toast because it says a great deal about nothing at all, on and on.",
        Tone::Info,
    );
    extra.push(("toasts".to_owned(), frame(&toasts)));
    let mut loading = base.clone();
    loading.data = None;
    extra.push(("loading".to_owned(), frame(&loading)));
    let mut busy = menu_state_with(paths, big, &["down", "down"], GlyphSet::Rich);
    busy.busy = Some("signing in acct2".to_owned());
    extra.push(("busy".to_owned(), frame(&busy)));
    let recent = menu_state_with(paths, big, &["end", "up", "up", "enter"], GlyphSet::Rich);
    extra.push(("recent-open".to_owned(), frame(&recent)));
    let fold = menu_state_with(paths, big, &["end", "up", "up"], GlyphSet::Rich);
    extra.push(("recent-fold".to_owned(), frame(&fold)));
    let search = menu_state_with(paths, big, &["up"], GlyphSet::Rich);
    extra.push(("search-focus".to_owned(), frame(&search)));
    let add = menu_state_with(paths, big, &["end", "down"], GlyphSet::Rich);
    extra.push(("add-focus".to_owned(), frame(&add)));
    let nothing = press(
        &menu_state_with(paths, big, &["/"], GlyphSet::Rich),
        paths,
        Action::Text("zzzz-no-matches".to_owned()),
    );
    extra.push(("filter-nothing".to_owned(), frame(&nothing.state)));
    let mode = menu_state_with(paths, big, &["down", "down", "enter", "m"], GlyphSet::Rich);
    extra.push(("mode".to_owned(), frame(&mode)));
    let add_dialog = menu_state_with(paths, big, &["end", "down", "enter"], GlyphSet::Rich);
    let typed = press(&add_dialog, paths, Action::Text("jane@example.com".to_owned()));
    extra.push(("add-dialog".to_owned(), frame(&typed.state)));
    let confirm = menu_state_with(
        paths,
        big,
        &[
            "end", "up", "enter", "down", "right", "down", "down", "down", "right", "end", "enter",
        ],
        GlyphSet::Rich,
    );
    extra.push(("policy-confirm".to_owned(), frame(&confirm)));
    // The keys on their page, and the Policy form where it is taller than its pane: a number being
    // typed, and the storage with the button under it.
    let small = Size { cols: 80, rows: 24 };
    for (name, size) in [("120x30", big), ("80x24", small)] {
        let keys = menu_state_with(paths, size, &["?"], GlyphSet::Rich);
        extra.push((format!("keys-{name}"), frame(&keys)));
    }
    let typing = menu_state_with(
        paths,
        small,
        &["end", "up", "enter", "down", "8", "5"],
        GlyphSet::Rich,
    );
    extra.push(("policy-typing-80x24".to_owned(), frame(&typing)));
    let storage = menu_state_with(paths, small, &["end", "up", "enter", "end"], GlyphSet::Rich);
    extra.push(("policy-storage-80x24".to_owned(), frame(&storage)));
    let thresholds = menu_state_with(paths, big, &["down", "down", "enter", "t", "7"], GlyphSet::Rich);
    extra.push(("thresholds-typing".to_owned(), frame(&thresholds)));
    let small_tall = menu_state_with(paths, Size { cols: 100, rows: 28 }, &["end"], GlyphSet::Rich);
    extra.push(("stats-100x28".to_owned(), frame(&small_tall)));
    let session2 = menu_state_with(
        paths,
        big,
        &["down", "down", "down", "down", "down", "down"],
        GlyphSet::Rich,
    );
    extra.push(("session-countdown".to_owned(), frame(&session2)));
    let waiting = menu_state_with(
        &blocked.paths,
        big,
        &["down", "down", "down", "down", "down", "down"],
        GlyphSet::Rich,
    );
    extra.push(("session-waiting".to_owned(), frame(&waiting)));
    // A home with long, wide and e-mail names, an account's own threshold and another policy; and a
    // home with no account at all.
    let long = "A very long account name that goes on and on, far past the width of the sidebar";
    let filter = "this is a very long filter text that does not fit in the search box at all";
    let odd = temp_home();
    build_home(&odd.paths, false);
    write_json(
        &odd.paths.config,
        &json!({
            "version": 1,
            "accounts": [
                { "id": "acct1", "name": long, "mark": "normal" },
                { "id": "acct2", "name": "日本語アカウント", "mark": "preferred" },
                { "id": "acct3", "name": "jane.doe+claude@example.com", "mark": "normal", "fiveHour": 80 },
                { "id": "acct4", "name": "acct4", "mark": "last-resort", "weekly": 90 }
            ],
            "policy": { "mode": "order", "usagePollMinutes": 0 }
        }),
    );
    let empty = temp_home();
    write_json(
        &empty.paths.config,
        &json!({ "version": 1, "accounts": [], "policy": {} }),
    );
    let keys = |names: &[&'static str]| names.iter().map(|name| Input::Key(name)).collect::<Vec<_>>();
    let down = |n: usize| vec![Input::Key("down"); n];
    let with = |mut first: Vec<Input>, rest: Vec<Input>| {
        first.extend(rest);
        first
    };
    let flows: Vec<(&str, Vec<Input>)> = vec![
        ("overview", vec![]),
        ("acct1", down(1)),
        ("acct2", down(2)),
        ("acct3", down(3)),
        ("acct4", down(4)),
        ("session", down(5)),
        ("session2", down(6)),
        ("recent", down(7)),
        ("recent-open", with(down(7), keys(&["enter", "up"]))),
        ("policy", keys(&["end", "up"])),
        (
            "policy-form",
            keys(&["end", "up", "enter", "right", "down", "left", "left"]),
        ),
        ("stats", keys(&["end"])),
        ("stats-scrolled", keys(&["end", "pagedown"])),
        ("actions1", keys(&["down", "enter"])),
        ("rename1", keys(&["down", "enter", "e"])),
        (
            "rename1-home",
            keys(&["down", "enter", "e", "home", "right", "right"]),
        ),
        (
            "thresholds3",
            with(down(3), keys(&["enter", "t", "right", "down", "left"])),
        ),
        ("remove1", keys(&["down", "enter", "d"])),
        ("hop-to", with(down(5), keys(&["enter", "H", "down"]))),
        ("mode2", keys(&["down", "down", "enter", "m", "down"])),
        ("filter-long", with(keys(&["/"]), vec![text(filter)])),
        ("filter-wide", with(keys(&["/"]), vec![text("日本")])),
        (
            "add-long",
            with(keys(&["end", "down", "enter"]), vec![text(long)]),
        ),
        (
            "add-taken",
            with(
                keys(&["end", "down", "enter"]),
                vec![text("ACCT4"), Input::Key("enter")],
            ),
        ),
    ];
    let play = |paths: &baton_core::paths::Paths, size: Size, inputs: &[Input]| {
        let mut state = menu_state_with(paths, size, &[], GlyphSet::Rich);
        for input in inputs {
            state = press(&state, paths, input.clone()).state;
        }
        state
    };
    for (size_name, size) in [("100x28", Size { cols: 100, rows: 28 }), ("120x30", big)] {
        for (name, inputs) in &flows {
            out.push((
                format!("y-{size_name}-{name}"),
                frame(&play(&odd.paths, size, inputs)),
            ));
        }
        let bare: Vec<(&str, Vec<Input>)> = vec![
            ("overview", vec![]),
            ("policy", keys(&["end", "up"])),
            ("stats", keys(&["end"])),
            ("add", keys(&["down", "down", "down", "enter"])),
            ("filter", with(keys(&["/"]), vec![text("pol")])),
        ];
        for (name, inputs) in &bare {
            out.push((
                format!("z-{size_name}-{name}"),
                frame(&play(&empty.paths, size, inputs)),
            ));
        }
    }
    for (name, rows) in &extra {
        out.push((format!("x-{name}"), rows.clone()));
    }
    out
}
