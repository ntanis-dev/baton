//! What the plain commands print, as lines of spans (`baton_core::term` draws them). The status, usage
//! and stats screens are the menu's own panes (the same view models and builders), so a command and
//! the menu always agree; the others are built from the same pieces.

use std::collections::BTreeMap;
use std::sync::Arc;

use baton_claude::ask::{choices, value_words};
use baton_claude::poller::PollResult;
use baton_core::clock::{Ms, parse_ms};
use baton_core::config::{Account, Config, Mark};
use baton_core::format::{TEXT_TIME, poll_line_of, poll_words};
use baton_core::json::show;
use baton_core::lines::{Line, p, s, spread, wrap_line};
use baton_core::stats::Stats;
use baton_core::term::{Terminal, banner, entry, fact, heading, mark, outcome, paragraph, room, tildify};
use baton_core::text::{pct, span, when};
use baton_install::install::{Applied, Item, Plan};
use baton_profile::accounts::{Added, Removed};
use baton_profile::copies::{BASE, Conflict};
use baton_profile::profile::LinkEntry;
use baton_profile::sync::SyncReport;
use baton_tui::data::MenuData;
use baton_tui::layout::Size;
use baton_tui::state::{Auth, Init, State, initial_state};
use baton_tui::tree::mark_icon;
use baton_tui::views::account::{MENU_TIME, account_body};
use baton_tui::views::common::reason_words;
use baton_tui::views::overview::overview_body;
use baton_tui::views::stats::stats_body;

use crate::cli::Command;

/// A count with its noun: `1 check`, `2 checks`.
fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// `text` padded with spaces to `width` characters.
fn pad_end(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.chars().count())))
}

/// `text` with spaces in front, up to `width` characters.
fn pad_start(text: &str, width: usize) -> String {
    format!("{}{text}", " ".repeat(width.saturating_sub(text.chars().count())))
}

// ---- doctor ------------------------------------------------------------------------------------

/// One line of `baton doctor`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// What was checked.
    pub name: String,
    /// Whether it is fine; None for a line that only informs.
    pub ok: Option<bool>,
    /// What was found.
    pub detail: String,
}

/// The checks of `baton doctor`, and how many failed.
pub fn doctor_screen(term: &Terminal, checks: &[Check]) -> Vec<Line> {
    let width = room(term);
    let name_width = checks
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(34);
    let failed = checks.iter().filter(|c| c.ok == Some(false)).count();
    let mut lines = banner(term, false);
    lines.push(heading("DOCTOR", width, term, ""));
    lines.push(Vec::new());
    for check in checks {
        let kind = match check.ok {
            Some(true) => "ok",
            Some(false) => "fail",
            None => "info",
        };
        let style = if check.ok == Some(false) { "text" } else { "muted" };
        lines.extend(entry(
            &mark(term.g, kind),
            &check.name,
            &[s(&check.detail, style)],
            width,
            name_width + 1,
        ));
    }
    lines.push(Vec::new());
    lines.push(if failed > 0 {
        outcome(term, &format!("{} failed.", plural(failed, "check")), "fail")
    } else {
        outcome(term, "All checks passed.", "done")
    });
    lines
}

// ---- help --------------------------------------------------------------------------------------

/// The groups the commands are listed under, with their titles.
pub const GROUPS: [(&str, &str); 5] = [
    ("session", "Conversation"),
    ("accounts", "Accounts"),
    ("usage", "Usage and policy"),
    ("setup", "Setup"),
    ("internal", "Run by Claude Code"),
];

/// Every command, or one command's usage.
pub fn help_screen(term: &Terminal, commands: &[Command], name: Option<&str>) -> Vec<Line> {
    let width = room(term);
    if let Some(name) = name {
        let Some(command) = commands.iter().find(|c| c.name == name) else {
            let mut lines = banner(term, false);
            lines.push(outcome(term, &format!("Unknown command {name}."), "fail"));
            lines.push(Vec::new());
            lines.extend(fact("Try", &[s("baton help", "accent bold")], width, 10, "muted"));
            return lines;
        };
        let mut lines = banner(term, false);
        lines.push(heading("USAGE", width, term, ""));
        lines.push(Vec::new());
        for usage in &command.usage {
            lines.extend(wrap_line(&[s(usage, "accent")], width, 4));
        }
        lines.push(Vec::new());
        lines.extend(paragraph(command.summary, width, "text"));
        if let Some(example) = command.example {
            lines.push(Vec::new());
            lines.push(heading("EXAMPLE", width, term, ""));
            lines.push(Vec::new());
            lines.push(vec![s(example, "accent")]);
        }
        return lines;
    }
    let names: Vec<&Command> = commands.iter().filter(|c| !c.alias).collect();
    let name_width = names.iter().map(|c| c.name.len()).max().unwrap_or(0).max(11) + 1;
    let mut lines = banner(term, true);
    lines.extend(wrap_line(
        &[
            s("baton", "accent bold"),
            s(" starts Claude Code on the best account; ", "muted"),
            s("baton view", "accent bold"),
            s(" opens the menu.", "muted"),
        ],
        width,
        0,
    ));
    for (group, title) in GROUPS {
        lines.push(Vec::new());
        lines.push(heading(&title.to_uppercase(), width, term, ""));
        lines.push(Vec::new());
        for command in names.iter().filter(|c| c.group == group) {
            lines.extend(wrap_line(
                &[
                    s(pad_end(command.name, name_width), "accent bold"),
                    s(command.summary, "text"),
                ],
                width,
                name_width,
            ));
        }
    }
    lines.push(Vec::new());
    lines.extend(
        wrap_line(
            &[
                s("baton help <command>", "accent"),
                s(" shows its options. ", "muted"),
                s("--json", "accent"),
                s(" prints data where it makes sense.", "muted"),
            ],
            width,
            0,
        )
        .into_iter()
        .take(1),
    );
    lines
}

// ---- accounts ----------------------------------------------------------------------------------

/// A mark in words: `last resort` for last-resort.
pub fn mark_word(mark: &str) -> &str {
    if mark == "last-resort" {
        "last resort"
    } else {
        mark
    }
}

/// The answer to an account command: a headline, facts, notes, an optional next step.
pub fn done_screen(
    term: &Terminal,
    title: &str,
    facts: &[(&str, String)],
    notes: &[&str],
    step: Option<&str>,
) -> Vec<Line> {
    let width = room(term);
    let mut lines = banner(term, false);
    lines.push(outcome(term, title, "done"));
    if !facts.is_empty() {
        lines.push(Vec::new());
    }
    for (label, value) in facts {
        lines.extend(fact(label, &[p(value)], width, 12, "muted"));
    }
    for note in notes {
        lines.push(Vec::new());
        lines.extend(paragraph(note, width, "muted"));
    }
    if let Some(step) = step {
        lines.push(Vec::new());
        lines.extend(baton_core::term::next(step, width, 12));
    }
    lines
}

/// The accounts in their new order.
pub fn account_order(term: &Terminal, order: &[String]) -> Vec<Line> {
    let width = room(term);
    let mut lines = banner(term, false);
    lines.push(outcome(term, "Order updated", "done"));
    lines.push(Vec::new());
    for (i, name) in order.iter().enumerate() {
        lines.extend(wrap_line(
            &[
                s(pad_start(&(i + 1).to_string(), 2), "muted"),
                p("  "),
                s(name, "bold"),
            ],
            width,
            4,
        ));
    }
    lines
}

// ---- no accounts, failures ---------------------------------------------------------------------

/// What to do when no account was added yet.
pub fn empty_screen(term: &Terminal) -> Vec<Line> {
    let width = room(term);
    let mut lines = banner(term, false);
    lines.push(outcome(term, "No accounts yet.", "skip"));
    lines.push(Vec::new());
    lines.extend(paragraph(
        "Add the Claude accounts Baton should use, then sign each one in with Claude Code.",
        width,
        "muted",
    ));
    lines.push(Vec::new());
    lines.extend(fact(
        "Add",
        &[s("baton account add <name>", "accent bold")],
        width,
        10,
        "muted",
    ));
    lines.extend(fact(
        "Sign in",
        &[s("baton login <name>", "accent bold")],
        width,
        10,
        "muted",
    ));
    lines
}

/// A command that could not do what was asked.
pub fn fail_screen(term: &Terminal, message: &str) -> Vec<Line> {
    let mut lines = banner(term, false);
    lines.extend(wrap_line(&outcome(term, message, "fail"), room(term), 2));
    lines
}

// ---- policy ------------------------------------------------------------------------------------

/// One setting of `baton policy`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyRow {
    /// The flag that changes it.
    pub flag: String,
    /// Its value, as shown.
    pub value: String,
    /// Whether the operator changed it.
    pub custom: bool,
}

/// The policy's settings, and how many were just saved.
pub fn policy_screen(term: &Terminal, rows: &[PolicyRow], mode: &str, saved: usize) -> Vec<Line> {
    let width = room(term);
    let flag_width = rows.iter().map(|r| r.flag.len()).max().unwrap_or(0);
    let value_width = rows.iter().map(|r| r.value.chars().count()).max().unwrap_or(0);
    let mut lines = banner(term, false);
    if saved > 0 {
        lines.push(outcome(
            term,
            &format!("Saved {}.", plural(saved, "setting")),
            "done",
        ));
        lines.push(Vec::new());
    }
    lines.push(heading("POLICY", width, term, &format!("{mode} mode")));
    lines.push(Vec::new());
    for row in rows {
        lines.push(vec![
            s(pad_end(&row.flag, flag_width + 2), "muted"),
            s(
                pad_end(&row.value, value_width),
                if row.custom { "accent bold" } else { "bold" },
            ),
            s(if row.custom { "  changed" } else { "  default" }, "muted"),
        ]);
    }
    lines
}

// ---- hop ---------------------------------------------------------------------------------------

/// A hop that was asked for.
pub fn hop_screen(term: &Terminal, session: &str, from: &str, to: &str, when: &str) -> Vec<Line> {
    let width = room(term);
    let mut lines = banner(term, false);
    lines.push(outcome(term, "Hop queued", "done"));
    lines.push(Vec::new());
    lines.extend(fact("Session", &[p(session)], width, 10, "muted"));
    lines.extend(fact("From", &[p(from)], width, 10, "muted"));
    lines.extend(fact("To", &[s(to, "accent bold")], width, 10, "muted"));
    lines.extend(fact("When", &[p(when)], width, 10, "muted"));
    lines
}

// ---- accounts: list, add, rename, remove, set ---------------------------------------------------

/// The accounts in order, with their marks and thresholds.
pub fn accounts_screen(term: &Terminal, config: &Config) -> Vec<Line> {
    if config.accounts.is_empty() {
        return empty_screen(term);
    }
    let width = room(term);
    let name_width = config
        .accounts
        .iter()
        .map(|a| a.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(40);
    let mut lines = banner(term, false);
    lines.push(heading(
        "ACCOUNTS",
        width,
        term,
        &plural(config.accounts.len(), "account"),
    ));
    lines.push(Vec::new());
    for (i, account) in config.accounts.iter().enumerate() {
        let thresholds: Vec<String> = [("5-hour", account.five_hour), ("weekly", account.weekly)]
            .into_iter()
            .filter_map(|(label, value)| Some(format!("{label} {}%", show(value?))))
            .collect();
        let mark = account.mark.as_str();
        let mut value = vec![s(
            mark_word(mark),
            if account.mark == Mark::Preferred {
                "baton"
            } else {
                "muted"
            },
        )];
        if !thresholds.is_empty() {
            value.push(s(format!("  {}", thresholds.join(" · ")), "muted"));
        }
        if account.id != account.name {
            value.push(s(format!("  folder {}", account.id), "muted"));
        }
        let head = [
            s(pad_start(&(i + 1).to_string(), 2), "muted"),
            p("  "),
            mark_icon(account.mark, term.g),
        ];
        lines.extend(entry(&head, &account.name, &value, width, name_width + 2));
    }
    lines
}

/// "linked 9 · copied 2": what adding an account did to its profile.
fn link_summary(links: &[LinkEntry]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for link in links.iter().filter(|link| link.action != "no-source") {
        match counts.iter_mut().find(|(action, _)| *action == link.action) {
            Some(entry) => entry.1 += 1,
            None => counts.push((link.action, 1)),
        }
    }
    counts
        .iter()
        .map(|(action, n)| format!("{action} {n}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// An account was added: where it is, and how to sign it in.
pub fn account_added(term: &Terminal, added: &Added, home: &str, login: &str) -> Vec<Line> {
    let mut facts = vec![
        ("Position", format!("#{}", added.position)),
        ("Mark", mark_word(added.account.mark.as_str()).to_owned()),
        ("Profile", tildify(&added.profile.to_string_lossy(), home)),
    ];
    let shared = link_summary(&added.links);
    if !shared.is_empty() {
        facts.push(("Shared", shared));
    }
    done_screen(
        term,
        &format!("Added {}", added.account.name),
        &facts,
        &[],
        Some(login),
    )
}

/// An account was renamed.
pub fn account_renamed(term: &Terminal, from: &str, to: &str) -> Vec<Line> {
    done_screen(
        term,
        &format!("Renamed {from} to {to}"),
        &[],
        &["Its profile, sign-in, sessions and history stay as they are."],
        None,
    )
}

/// An account was removed from Baton; its folder stays.
pub fn account_removed(term: &Terminal, removed: &Removed, home: &str) -> Vec<Line> {
    let unlinked = if removed.unlinked.is_empty() {
        "none".to_owned()
    } else {
        format!("{} shared", removed.unlinked.len())
    };
    done_screen(
        term,
        &format!("Removed {} from Baton", removed.name),
        &[
            ("Unlinked", unlinked),
            ("Folder", tildify(&removed.profile.to_string_lossy(), home)),
        ],
        &["Its folder still holds that account's sign-in; delete it yourself when you no longer need it."],
        None,
    )
}

/// An account's mark or thresholds were changed.
pub fn account_set(term: &Terminal, account: &Account) -> Vec<Line> {
    let percent =
        |value: Option<f64>| value.map_or_else(|| "default".to_owned(), |n| format!("{}%", show(n)));
    done_screen(
        term,
        &format!("Updated {}", account.name),
        &[
            ("Mark", mark_word(account.mark.as_str()).to_owned()),
            ("5-hour", percent(account.five_hour)),
            ("Weekly", percent(account.weekly)),
        ],
        &[],
        None,
    )
}

// ---- conflicts ---------------------------------------------------------------------------------

/// The shared files that changed in two places at once; `cleared` says how many were just cleared.
pub fn conflicts_screen(
    term: &Terminal,
    list: &[Conflict],
    label: &dyn Fn(&str) -> String,
    cleared: Option<usize>,
) -> Vec<Line> {
    let width = room(term);
    let mut lines = banner(term, false);
    if let Some(cleared) = cleared {
        lines.push(outcome(
            term,
            &format!("Cleared {}.", plural(cleared, "conflict")),
            "done",
        ));
        return lines;
    }
    if list.is_empty() {
        lines.push(outcome(term, "No conflicts.", "same"));
        return lines;
    }
    lines.push(heading("CONFLICTS", width, term, &plural(list.len(), "conflict")));
    lines.push(Vec::new());
    let indented = |rows: Vec<Line>| {
        rows.into_iter().map(|row| {
            let mut line = vec![p("  ")];
            line.extend(row);
            line
        })
    };
    for (i, conflict) in list.iter().enumerate() {
        if i > 0 {
            lines.push(Vec::new());
        }
        let mut head = mark(term.g, "warn");
        head.push(p(" "));
        head.push(s(
            format!("{} in {}", conflict.item, label(&conflict.account)),
            "bold",
        ));
        head.push(s(
            format!("  {}", conflict.at.as_deref().unwrap_or_default()),
            "muted",
        ));
        lines.extend(wrap_line(&head, width, 2));
        let kept = if conflict.kept == BASE {
            BASE.to_owned()
        } else {
            label(&conflict.kept)
        };
        lines.extend(indented(fact(
            "Kept",
            &[p(format!("{kept}'s version"))],
            width,
            8,
            "muted",
        )));
        lines.extend(indented(fact(
            "Backup",
            &[p(conflict.backup.as_deref().unwrap_or_default())],
            width,
            8,
            "muted",
        )));
    }
    lines.push(Vec::new());
    lines.extend(fact(
        "Clear",
        &[s("baton conflicts --clear", "accent")],
        width,
        10,
        "muted",
    ));
    lines
}

// ---- install and uninstall ----------------------------------------------------------------------

/// The pieces in the order they are listed.
const ITEM_ORDER: [&str; 5] = ["statusline", "hooks", "hop", "ferry", "agent"];

/// When Ferry is not on this machine, the plan says so in one line.
const FERRY_MISSING: &str = "Ferry not found; run baton install again after installing it";

/// A piece of the install by name.
fn item_label(name: &str) -> &'static str {
    match name {
        "statusline" => "Status line",
        "hooks" => "Hooks",
        "hop" => "/hop command",
        "ferry" => "Ferry",
        _ => "Ferry's agent",
    }
}

/// What happened to a piece (done) or will (a dry run), in words.
fn item_words(name: &str, action: &str) -> (&'static str, &'static str) {
    match (name, action) {
        // Baton's launcher in Ferry is registered, not installed.
        ("ferry", "add") => ("registered", "will register"),
        ("ferry", "absent") => ("not registered", "not registered"),
        // The agent Ferry opens with `c` is set, kept or put back, not installed.
        ("agent", "set") => ("now Baton", "will be Baton"),
        ("agent", "same") => ("Baton", "Baton"),
        ("agent", "keep") => ("left alone", "left alone"),
        ("agent", "restore") => ("put back", "will put back"),
        ("agent", "absent") => ("not Baton", "not Baton"),
        ("agent", "skip") => ("not changed", "not changed"),
        (_, "add") => ("added", "will add"),
        (_, "update") => ("updated", "will update"),
        (_, "remove") => ("removed", "will remove"),
        (_, "same") => ("in place", "in place"),
        (_, "absent") => ("not installed", "not installed"),
        (_, "skip") => ("left alone", "left alone"),
        _ => ("failed", "failed"),
    }
}

/// What there is to say about a piece beyond the word.
fn item_detail(item: &Item, mode: &str, home: &str) -> String {
    if item.action == "absent" {
        return String::new();
    }
    match item.name {
        "statusline" if mode == "uninstall" => match (item.action, item.restored) {
            ("remove", Some(true)) => "your previous status line is back".to_owned(),
            ("remove", _) => "removed from settings.json".to_owned(),
            _ => String::new(),
        },
        "statusline" => "baton statusline".to_owned(),
        "hooks" => item.events.as_deref().unwrap_or_default().join(" · "),
        "agent" => match item.action {
            "set" => format!("was {}", item.previous.as_deref().unwrap_or_default()),
            "same" => "c opens Baton".to_owned(),
            "keep" => format!("{}, as you chose", item.current.as_deref().unwrap_or_default()),
            "restore" => item.previous.clone().unwrap_or_default(),
            "skip" => item.why.unwrap_or_default().to_owned(),
            _ => String::new(),
        },
        _ => item
            .path
            .as_deref()
            .map(|path| tildify(&path.to_string_lossy(), home))
            .unwrap_or_default(),
    }
}

/// What `baton install` or `baton uninstall` shows.
#[derive(Clone, Copy, Debug)]
pub struct PlanView<'a> {
    /// `install` or `uninstall`.
    pub mode: &'a str,
    /// The plan.
    pub plan: &'a Plan,
    /// Whether nothing was written.
    pub dry_run: bool,
    /// What applying it did.
    pub applied: Option<&'a Applied>,
    /// Whether to list every changed line.
    pub diff: bool,
    /// The home folder, shown as ~.
    pub home: &'a str,
    /// Whether an account was added already.
    pub has_accounts: bool,
}

/// The plan and what came of it.
pub fn plan_screen(term: &Terminal, view: &PlanView<'_>) -> Vec<Line> {
    let PlanView {
        mode,
        plan,
        dry_run,
        applied,
        diff,
        home,
        has_accounts,
    } = *view;
    let width = room(term);
    let g = term.g;
    let install = mode == "install";
    let mut lines = banner(term, install);
    lines.push(heading(
        &mode.to_uppercase(),
        width,
        term,
        if dry_run { "dry run" } else { "" },
    ));
    lines.push(Vec::new());
    let (label_width, word_width) = (14, 14);
    let mut items: Vec<&Item> = plan.items.iter().collect();
    items.sort_by_key(|item| ITEM_ORDER.iter().position(|name| *name == item.name));
    for item in items {
        if item.action == "missing" {
            let mut line = mark(g, "skip");
            line.extend([p(" "), s(FERRY_MISSING, "muted")]);
            lines.extend(wrap_line(&line, width, 2));
            continue;
        }
        let changing = ["add", "update", "remove", "set", "restore"].contains(&item.action);
        let mut line = if !changing {
            mark(g, if item.action == "same" { "same" } else { "skip" })
        } else if dry_run {
            vec![s(g.arrow, "accent bold")]
        } else {
            mark(g, "done")
        };
        let words = item_words(item.name, item.action);
        let word = if dry_run { words.1 } else { words.0 };
        let tone = match (changing, dry_run) {
            (true, true) => "accent bold",
            (true, false) => "ok bold",
            (false, _) => "muted",
        };
        line.extend([
            p(" "),
            s(pad_end(item_label(item.name), label_width), "text bold"),
            s(pad_end(word, word_width), tone),
            s(item_detail(item, mode, home), "muted"),
        ]);
        lines.extend(wrap_line(&line, width, 2 + label_width + word_width));
    }
    for note in &plan.notes {
        lines.push(Vec::new());
        let mut line = mark(g, "warn");
        line.extend([p(" "), s(note, "warn")]);
        lines.extend(wrap_line(&line, width, 2));
    }
    if !plan.files.is_empty() {
        lines.push(Vec::new());
        lines.push(heading("FILES", width, term, ""));
        lines.push(Vec::new());
        for file in &plan.files {
            let added = file.diff.iter().filter(|line| line.starts_with("+ ")).count();
            let removed = file.diff.iter().filter(|line| line.starts_with("- ")).count();
            let what = match (&file.after, &file.before) {
                (None, _) => "remove",
                (_, None) => "create",
                _ => "change",
            };
            let mut counts = Vec::new();
            if added > 0 {
                counts.push(s(format!("+{added}"), "ok bold"));
            }
            if added > 0 && removed > 0 {
                counts.push(p(" "));
            }
            if removed > 0 {
                counts.push(s(format!("{}{removed}", g.less), "err bold"));
            }
            lines.push(spread(
                &[
                    s(pad_end(what, 8), "muted"),
                    s(tildify(&file.path.to_string_lossy(), home), "text"),
                ],
                &counts,
                width,
                "",
            ));
            if diff {
                for line in &file.diff {
                    let style = match line.chars().next() {
                        Some('+') => "ok",
                        Some('-') => "err",
                        _ => "muted",
                    };
                    lines.extend(wrap_line(&[s(format!("    {line}"), style)], width, 6));
                }
            }
        }
        if !diff {
            lines.push(Vec::new());
            lines.push(vec![
                s("Add ", "muted"),
                s("--diff", "accent"),
                s(" to see every changed line.", "muted"),
            ]);
        }
    }
    lines.push(Vec::new());
    match applied {
        _ if dry_run => {
            lines.push(outcome(term, "Dry run: nothing was written.", "info"));
            lines.push(Vec::new());
            lines.extend(fact(
                "Apply",
                &[s(format!("baton {mode}"), "accent bold")],
                width,
                10,
                "muted",
            ));
        }
        Some(applied) if applied.changed => {
            lines.push(outcome(
                term,
                if install { "Installed" } else { "Uninstalled" },
                "done",
            ));
            lines.push(Vec::new());
            if let Some(backup) = &applied.backup {
                lines.extend(fact(
                    "Backup",
                    &[p(tildify(&backup.to_string_lossy(), home))],
                    width,
                    10,
                    "muted",
                ));
            }
            if install {
                lines.extend(fact("Undo", &[p("baton uninstall")], width, 10, "muted"));
                let step = if has_accounts {
                    "baton"
                } else {
                    "baton account add <name>, then baton login <name>"
                };
                lines.extend(baton_core::term::next(step, width, 10));
            } else {
                lines.extend(fact("Again", &[p("baton install")], width, 10, "muted"));
            }
        }
        _ => lines.push(if install {
            outcome(term, "Already installed. Nothing to change.", "same")
        } else {
            outcome(term, "Not installed. Nothing to change.", "skip")
        }),
    }
    lines
}

// ---- The menu's panes: status, usage, stats ------------------------------------------------------

/// The menu's state for the command line: its data, and the sign-in of each account as far as the
/// usage poll knows it (asking Claude Code for it is baton doctor's job).
fn menu_state(term: &Terminal, data: MenuData) -> State {
    let mut auth = BTreeMap::new();
    if let Some(poll) = &data.poll {
        for (id, entry) in &poll.state.accounts {
            let logged_in = match entry.status.as_deref() {
                Some("ok") => true,
                Some("signed-out") => false,
                _ => continue,
            };
            auth.insert(
                id.clone(),
                Auth {
                    logged_in: Some(logged_in),
                    ..Auth::default()
                },
            );
        }
    }
    let mut state = initial_state(Init {
        size: Size {
            cols: term.cols,
            rows: 40,
        },
        now: data.now,
        color: term.styled,
        glyphs: term.glyphs,
        data: Some(Arc::new(data)),
        splash: false,
    });
    state.auth = auth;
    state
}

/// The usage poll in one line, as a paragraph.
fn poll_paragraph(data: &MenuData, width: usize) -> Vec<Line> {
    let line = poll_line_of(data.now, data.poll.as_ref(), &data.policy, &data.states).unwrap_or_default();
    paragraph(&line, width, "muted")
}

/// `baton status`: the menu's Overview, and what the usage poll has to say.
pub fn status_screen(term: &Terminal, data: MenuData) -> Vec<Line> {
    if data.states.is_empty() {
        return empty_screen(term);
    }
    let width = room(term);
    let state = menu_state(term, data);
    let Some(data) = state.data.as_deref() else {
        return empty_screen(term);
    };
    let mut lines = banner(term, false);
    lines.extend(poll_paragraph(data, width));
    lines.push(Vec::new());
    lines.extend(overview_body(&state, width));
    // The accounts whose usage poll has something to say.
    let words = |id: &str| {
        let entry = data.poll.as_ref()?.state.accounts.get(id)?;
        let status = entry.status.as_deref().filter(|status| !status.is_empty())?;
        (status != "ok").then(|| poll_words(Some(entry), state.now, MENU_TIME))
    };
    let trouble: Vec<Line> = data
        .states
        .iter()
        .filter_map(|account| {
            Some(vec![
                s(&account.name, "bold"),
                p("  "),
                s(words(&account.id)?, "muted"),
            ])
        })
        .collect();
    if !trouble.is_empty() {
        lines.push(Vec::new());
        lines.push(heading("USAGE POLL", width, term, ""));
        for line in &trouble {
            lines.extend(wrap_line(line, width, 2));
        }
    }
    lines.push(Vec::new());
    lines.push(vec![
        s("baton usage", "accent"),
        s(" shows every window and where it came from", "muted"),
    ]);
    lines
}

/// `baton usage`: every account's windows (or one account's), and the usage poll.
pub fn usage_screen(term: &Terminal, data: MenuData, account: Option<&str>) -> Vec<Line> {
    if data.states.is_empty() {
        return empty_screen(term);
    }
    let width = room(term);
    let g = term.g;
    let state = menu_state(term, data);
    let Some(data) = state.data.as_deref() else {
        return empty_screen(term);
    };
    let mut lines = banner(term, false);
    let shown = data.states.iter().filter(|a| account.is_none_or(|id| a.id == id));
    for (i, a) in shown.enumerate() {
        if i > 0 {
            lines.push(Vec::new());
        }
        let standing = if !a.limited_models.is_empty() {
            s(
                format!("{} {} limit", g.blocked, a.limited_models.join(", ")),
                "warn",
            )
        } else if a.blocked {
            s(format!("{} blocked", g.blocked), "err")
        } else {
            s("available", "ok")
        };
        lines.push(spread(&[s(&a.name, "bold")], &[standing], width, ""));
        lines.push(vec![s(g.corners.h.repeat(width), "divider")]);
        lines.extend(account_body(&state, &a.id, width));
    }
    lines.push(Vec::new());
    lines.extend(poll_paragraph(data, width));
    lines
}

/// `baton stats`: the menu's Stats over the days asked for.
pub fn stats_screen(term: &Terminal, mut data: MenuData, stats: Stats) -> Vec<Line> {
    let days = stats.days.len();
    data.stats = stats;
    let width = room(term);
    let state = menu_state(term, data);
    let mut lines = banner(term, false);
    lines.push(heading("STATS", width, term, &format!("last {days} days")));
    lines.push(Vec::new());
    lines.extend(stats_body(&state, width));
    lines
}

// ---- usage --refresh, pick ------------------------------------------------------------------------

/// What asking Claude Code for every account's usage came to.
pub fn refresh_screen(term: &Terminal, results: &[PollResult], now: Ms) -> Vec<Line> {
    let width = room(term);
    let name_width = results
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(40);
    let mut lines = banner(term, false);
    lines.push(heading("REFRESH", width, term, ""));
    lines.push(Vec::new());
    for result in results {
        if result.status == "ok" {
            let reading = result.reading.as_ref();
            let parts: Vec<String> = [
                ("5-hour", reading.and_then(|r| r.five_hour.as_ref())),
                ("weekly", reading.and_then(|r| r.seven_day.as_ref())),
            ]
            .into_iter()
            .filter_map(|(label, window)| Some(format!("{label} {}", pct(Some(window?.used)))))
            .collect();
            let said = if parts.is_empty() {
                "no meters reported".to_owned()
            } else {
                parts.join(" · ")
            };
            lines.extend(entry(
                &mark(term.g, "ok"),
                &result.name,
                &[p(said), s("  polled now", "muted")],
                width,
                name_width,
            ));
            continue;
        }
        let text = match result.status.as_str() {
            "backoff" => {
                let until = result.until.as_deref().and_then(parse_ms);
                format!(
                    "not asked: the usage endpoint was rate limited; asks again {} (in {})",
                    when(until, now),
                    span(until.map(|until| until - now))
                )
            }
            "unknown" => "not one of Baton's accounts".to_owned(),
            _ => poll_words(Some(&result.entry()), now, TEXT_TIME),
        };
        let kind = if result.status == "unknown" {
            "fail"
        } else {
            "skip"
        };
        lines.extend(entry(
            &mark(term.g, kind),
            &result.name,
            &[s(text, "muted")],
            width,
            name_width,
        ));
    }
    lines
}

/// Where new work would go now.
pub fn pick_screen(
    term: &Terminal,
    name: Option<&str>,
    reason: &str,
    earliest: Option<&str>,
    at: Option<Ms>,
    now: Ms,
) -> Vec<Line> {
    let width = room(term);
    let mut lines = banner(term, false);
    let line = match name {
        Some(name) => {
            let mut line = mark(term.g, "ok");
            line.extend([
                p(" "),
                s(name, "bold"),
                s(format!("  {}", reason_words(reason)), "muted"),
            ]);
            line
        }
        None => {
            let mut line = vec![
                s(term.g.blocked, "warn bold"),
                p(" "),
                s("Every account is at its limit", "bold"),
            ];
            if let Some(earliest) = earliest {
                let back = match at.filter(|at| *at != 0) {
                    Some(at) => format!(" at {} (in {})", when(Some(at), now), span(Some(at - now))),
                    None => String::new(),
                };
                line.push(s(format!("  {earliest} is first back{back}"), "muted"));
            }
            line
        }
    };
    lines.extend(wrap_line(&line, width, 2));
    lines
}

// ---- sync ---------------------------------------------------------------------------------------

/// What `baton sync` shows.
#[derive(Clone, Copy)]
pub struct SyncView<'a> {
    /// Whether full sync is on.
    pub on: bool,
    /// What the sync did; None when it is off.
    pub report: Option<&'a SyncReport>,
    /// For an answer just given: how many settings, and whose value they took.
    pub took: Option<(usize, &'a str)>,
    /// The name of a participant: an account's, or `~/.claude.json`.
    pub name: &'a dyn Fn(&str) -> String,
}

impl std::fmt::Debug for SyncView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncView")
            .field("on", &self.on)
            .field("took", &self.took)
            .finish_non_exhaustive()
    }
}

/// `baton sync`: whether the accounts are in step, and what only the operator can settle.
pub fn sync_screen(term: &Terminal, view: &SyncView<'_>) -> Vec<Line> {
    let width = room(term);
    let mut lines = banner(term, false);
    let Some(report) = view.report.filter(|_| view.on) else {
        lines.push(outcome(term, "Full sync is off.", "skip"));
        lines.push(Vec::new());
        lines.extend(paragraph(
            "Each account keeps its own preferences, MCP servers and folder approvals, and starts from ~/.claude.json.",
            width,
            "muted",
        ));
        lines.push(Vec::new());
        lines.extend(fact(
            "Turn on",
            &[s("baton policy --full-sync on", "accent bold")],
            width,
            10,
            "muted",
        ));
        return lines;
    };
    if report.busy {
        lines.push(outcome(
            term,
            "Another baton is syncing the settings; try again in a moment.",
            "fail",
        ));
        return lines;
    }
    if let Some((count, who)) = view.took {
        lines.push(outcome(
            term,
            &format!("Took {who}'s value for {}.", plural(count, "setting")),
            "done",
        ));
    }
    if report.written.is_empty() {
        lines.push(outcome(term, "Every account is in step.", "same"));
    } else {
        lines.push(outcome(
            term,
            &format!("Brought {} in step.", plural(report.written.len(), "account")),
            "done",
        ));
        lines.push(Vec::new());
        let names: Vec<String> = report
            .written
            .iter()
            .map(|written| (view.name)(&written.account))
            .collect();
        let name_width = names
            .iter()
            .map(|name| name.chars().count())
            .max()
            .unwrap_or(0)
            .min(40)
            + 2;
        for (written, name) in report.written.iter().zip(&names) {
            lines.extend(fact(
                name,
                &[p(plural(written.settings, "setting"))],
                width,
                name_width,
                "bold",
            ));
        }
    }
    let mut warn = |text: String| {
        lines.push(Vec::new());
        let mut line = mark(term.g, "warn");
        line.extend([p(" "), s(text, "warn")]);
        lines.extend(wrap_line(&line, width, 2));
    };
    for id in &report.reset {
        warn(format!(
            "{}'s settings were started afresh by Claude Code; it gets the accounts' settings back.",
            (view.name)(id)
        ));
    }
    for id in &report.unreadable {
        warn(format!(
            "{}'s .claude.json cannot be read; it is left as it is.",
            (view.name)(id)
        ));
    }
    if report.questions.is_empty() {
        return lines;
    }
    lines.push(Vec::new());
    lines.push(heading(
        "THE ACCOUNTS DIFFER",
        width,
        term,
        &plural(report.questions.len(), "setting"),
    ));
    for (at, question) in report.questions.iter().enumerate() {
        lines.push(Vec::new());
        lines.extend(wrap_line(
            &[
                s(pad_start(&(at + 1).to_string(), 2), "accent bold"),
                p("  "),
                s(&question.label, "bold"),
            ],
            width,
            4,
        ));
        let offered = choices(question);
        let value_width = offered
            .iter()
            .map(|(value, _)| value_words(value.as_ref()).chars().count())
            .max()
            .unwrap_or(0);
        for (value, holders) in &offered {
            let who: Vec<String> = holders.iter().map(|who| (view.name)(who)).collect();
            lines.extend(wrap_line(
                &[
                    p("    "),
                    p(pad_end(&value_words(value.as_ref()), value_width + 2)),
                    s(who.join(", "), "muted"),
                ],
                width,
                6 + value_width,
            ));
        }
    }
    lines.push(Vec::new());
    lines.extend(fact(
        "Answer",
        &[s("baton sync take <number> <account>", "accent bold")],
        width,
        10,
        "muted",
    ));
    lines.extend(paragraph(
        "That account's value goes to every account. Until then nobody's is written over.",
        width,
        "muted",
    ));
    lines
}
