//! The commands of `baton`, in one table.

use std::panic::{AssertUnwindSafe, catch_unwind};

use baton_claude::lead::{LeadOptions, run_lead};
use baton_claude::poller::{Poller, PollerOptions};
use baton_core::claude_path::resolve_claude;
use baton_core::clock::iso;
use baton_core::config::{
    By, Config, Policy, effective_policy, find_account, load_config, loose_f64, name_of,
};
use baton_core::format::{NO_ACCOUNTS, status_view};
use baton_core::hop::{HopRequest, hop};
use baton_core::json::{Get, Object, num, parse, show};
use baton_core::paths::{Paths, profile_env};
use baton_core::policy::pick_start;
use baton_core::poll::request_poll;
use baton_core::state::{Want, load_snapshot};
use baton_core::stats::compute_stats;
use baton_install::hooks::{
    StatusLine, append_usage, columns_of, config_notice, handle_hook, handle_status_line,
    run_previous_status_line,
};
use baton_install::install::{apply_plan, plan_install, plan_uninstall};
use baton_profile::accounts::{
    AccountChanges, AccountError, AddOptions, account_json, add_account, move_account, order_accounts,
    remove_account, rename_account, set_account, set_policy,
};
use baton_profile::copies::{clear_conflicts, read_conflicts};
use baton_profile::profile::{LinkOptions, link_profile};
use baton_profile::sync::{Apply, HOME, full_sync, sync_settings, take};
use baton_tui::data::load_menu_data;
use baton_tui::{MenuOptions, SignInOptions, run_menu, sign_in};
use serde_json::{Value, json};

use crate::args::Spec;
use crate::cli::{CliError, Command, Ctx, Outcome, usage};
use crate::doctor::{DoctorOptions, run_doctor};
use crate::screens::{self, PlanView, PolicyRow, SyncView};

impl From<AccountError> for CliError {
    fn from(error: AccountError) -> CliError {
        match error {
            AccountError::Invalid(message) => CliError::Usage(message),
            AccountError::Io(error) => CliError::Failed(error.to_string()),
        }
    }
}

/// A value as JSON for `--json`; null when it cannot be one.
fn data<T: serde::Serialize>(value: &T) -> Option<Value> {
    serde_json::to_value(value).ok()
}

/// A path as the text the screens show.
fn text_of(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Reads a true-or-false option.
fn bool_option(value: Option<&str>, flag: &str) -> Result<Option<bool>, CliError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value.to_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(Some(true)),
        "false" | "off" | "no" | "0" => Ok(Some(false)),
        _ => Err(usage(format!("--{flag} takes true or false."))),
    }
}

/// The characters a name may hold and still be typed in a shell as it is.
const SHELL_PLAIN: &str = "@._+:,=/-";

/// The characters that are escaped inside double quotes.
const SHELL_ESCAPED: [char; 4] = ['"', '\\', '$', '`'];

/// A name as it would be typed in a shell (names may hold spaces and more).
pub fn quoted(name: &str) -> String {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || SHELL_PLAIN.contains(c));
    if plain {
        return name.to_owned();
    }
    let mut out = String::from("\"");
    for c in name.chars() {
        if SHELL_ESCAPED.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Reads a threshold option: a percentage, or "default" to go back to the policy's.
fn percent_or_default(value: Option<&str>, flag: &str) -> Result<Option<Value>, CliError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.eq_ignore_ascii_case("default") {
        return Ok(Some(Value::Null));
    }
    let n = loose_f64(&Value::String(value.to_owned()));
    if !n.is_finite() {
        return Err(usage(format!("--{flag} takes a percentage or \"default\".")));
    }
    Ok(Some(num(n)))
}

/// A command with no options that reads its arguments.
fn command(
    name: &'static str,
    group: &'static str,
    usage: &[&str],
    summary: &'static str,
    run: fn(&mut Ctx<'_, '_>) -> Result<Outcome, CliError>,
) -> Command {
    Command {
        name,
        group,
        usage: usage.iter().map(|line| (*line).to_owned()).collect(),
        summary,
        example: None,
        options: Vec::new(),
        raw: false,
        alias: false,
        prepare: None,
        run,
    }
}

/// The answer of `baton help [command]`.
pub fn help_outcome(name: Option<&str>) -> Outcome {
    let table = commands();
    let known = name.is_none_or(|name| table.iter().any(|c| c.name == name));
    let name = name.map(str::to_owned);
    Outcome {
        screen: Some(Box::new(move |term| {
            screens::help_screen(term, &table, name.as_deref())
        })),
        code: if known { 0 } else { 2 },
        ..Outcome::default()
    }
}

// ---- Claude Code's callers: hooks and the status line -------------------------------------------

/// `baton hook <event>`: record a Claude Code hook event from the JSON on stdin.
fn run_hook(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let input = parse(&ctx.io.read_stdin()).unwrap_or(Value::Null);
    let event = ctx.values.positional(0).unwrap_or_default();
    let (env, paths, now) = (&ctx.io.env, &ctx.paths, (ctx.io.now)());
    // A hook never fails the session.
    let _ = catch_unwind(AssertUnwindSafe(|| handle_hook(event, &input, env, paths, now)));
    // A config that cannot be used makes this profile nobody's account: say so where it can be read.
    if let Some(notice) = config_notice(env, paths) {
        let _ = writeln!(ctx.io.stderr, "{notice}");
    }
    Ok(Outcome::quiet(0))
}

/// `baton statusline`: record rate_limits and print the status line.
fn run_statusline(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let raw = ctx.io.read_stdin();
    let input = parse(&raw).unwrap_or(Value::Null);
    let (env, paths, now) = (&ctx.io.env, &ctx.paths, (ctx.io.now)());
    let result = catch_unwind(AssertUnwindSafe(|| handle_status_line(&input, env, paths, now)))
        .unwrap_or_else(|_| StatusLine::Text(String::new()));
    match result {
        StatusLine::Previous { command, usage } => {
            let out = run_previous_status_line(&command, &raw, env);
            // Their line as it is, or with Baton's usage after it (policy statusLineUsage).
            let text = match usage {
                Some(usage) => append_usage(&out.stdout, &|room| usage.line(room), columns_of(env)),
                None => out.stdout,
            };
            let _ = write!(ctx.io.stdout, "{text}");
            Ok(Outcome::quiet(out.code))
        }
        StatusLine::Text(text) => {
            // Nothing to show because the config cannot be used: that is what the line says.
            let text = match config_notice(env, paths) {
                Some(notice) if text.is_empty() => notice,
                _ => text,
            };
            if !text.is_empty() {
                let _ = writeln!(ctx.io.stdout, "{text}");
            }
            Ok(Outcome::quiet(0))
        }
    }
}

// ---- The conversation ---------------------------------------------------------------------------

/// `baton [claude arguments...]`: Claude Code on the best account, the conversation moved on when one
/// runs out.
fn run_start(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let options = LeadOptions::new(ctx.rest.clone(), ctx.paths.clone(), ctx.io.env.clone());
    Ok(Outcome::quiet(run_lead(options)))
}

/// `baton hop`: move a conversation running under baton at the end of its turn.
fn run_hop(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let from_command = ctx.values.flag("from-command");
    let request = HopRequest {
        session: ctx.values.text("session").or_else(|| ctx.values.positional(0)),
        to: ctx.values.text("to").filter(|to| !to.is_empty()),
        to_by: By::Name,
        env: &ctx.io.env,
        by: if from_command { "command" } else { "cli" },
        now: (ctx.io.now)(),
    };
    match hop(&ctx.paths, &request) {
        Ok(done) => {
            let when = if done.between {
                "now (it is between turns)"
            } else {
                "when this turn ends"
            };
            // The /hop command's answer goes to the model: plain words that name no account.
            if from_command {
                return Ok(Outcome {
                    text: Some(format!(
                        "Baton: this conversation moves to another account {when}."
                    )),
                    ..Outcome::default()
                });
            }
            // A script reads plain words too; a terminal gets the screen.
            let words = format!(
                "Baton: conversation {} moves from {} to {} {when}.",
                done.session, done.from, done.to
            );
            let data = data(&done);
            Ok(Outcome {
                data,
                text: Some(words.clone()),
                pipe: Some(words),
                screen: Some(Box::new(move |term| {
                    screens::hop_screen(term, &done.session, &done.from, &done.to, when)
                })),
                ..Outcome::default()
            })
        }
        Err(error) => {
            // The /hop command shows Baton's answer to the model: it is not a failure there, and it
            // names no account.
            if from_command {
                return Ok(Outcome {
                    text: Some(format!("Baton: {}", error.unnamed())),
                    ..Outcome::default()
                });
            }
            let message = error.message;
            let data = Some(json!({ "error": message }));
            let text = Some(format!("Baton: {message}"));
            Ok(Outcome {
                data,
                text,
                screen: Some(Box::new(move |term| screens::fail_screen(term, &message))),
                code: 1,
                ..Outcome::default()
            })
        }
    }
}

// ---- Accounts -----------------------------------------------------------------------------------

/// `--last-resort` alone means true on add.
fn prepare_account(args: &[String]) -> Vec<String> {
    args.iter()
        .enumerate()
        .map(|(i, arg)| {
            let alone = args.get(i + 1).is_none_or(|next| next.starts_with('-'));
            if arg == "--last-resort" && alone {
                "--last-resort=true".to_owned()
            } else {
                arg.clone()
            }
        })
        .collect()
}

/// `baton account ...`: the accounts, their order, marks and thresholds.
fn run_account(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let paths = ctx.paths.clone();
    let home = text_of(&paths.home);
    let now = (ctx.io.now)();
    let values = &ctx.values;
    let names: Vec<&str> = values.positionals.iter().skip(1).map(String::as_str).collect();
    match values.positional(0) {
        None | Some("list") => {
            let config = load_config(&paths);
            let list: Vec<Value> = config.accounts.iter().map(account_json).collect();
            Ok(Outcome::shown(
                Value::Array(list),
                Box::new(move |term| screens::accounts_screen(term, &config)),
            ))
        }
        Some("add") => {
            let Some(name) = names.first() else {
                return Err(usage("baton account add <name>"));
            };
            let last_resort = bool_option(values.text("last-resort"), "last-resort")?;
            let options = AddOptions {
                mark: if last_resort == Some(true) {
                    Some("last-resort".to_owned())
                } else {
                    values.text("mark").map(str::to_owned)
                },
                position: values
                    .text("position")
                    .map(|position| Value::String(position.to_owned())),
                file_links: None,
            };
            let added = add_account(&paths, name, &options)?;
            // A new or renamed account is polled by the baton instance that polls, if one runs.
            request_poll(&paths, &added.account.id, "added", now);
            let login = format!("baton login {}", quoted(&added.account.name));
            Ok(Outcome {
                data: data(&added),
                screen: Some(Box::new(move |term| {
                    screens::account_added(term, &added, &home, &login)
                })),
                ..Outcome::default()
            })
        }
        Some("rename") => {
            let [name, new_name] = names.as_slice() else {
                return Err(usage("baton account rename <name> <new name>"));
            };
            let renamed = rename_account(&paths, name, new_name, By::Name)?;
            request_poll(&paths, &renamed.id, "renamed", now);
            Ok(Outcome {
                data: data(&renamed),
                screen: Some(Box::new(move |term| {
                    screens::account_renamed(term, &renamed.from, &renamed.to)
                })),
                ..Outcome::default()
            })
        }
        Some("remove") => {
            let Some(name) = names.first() else {
                return Err(usage("baton account remove <name>"));
            };
            let removed = remove_account(&paths, name, By::Name)?;
            Ok(Outcome {
                data: data(&removed),
                screen: Some(Box::new(move |term| {
                    screens::account_removed(term, &removed, &home)
                })),
                ..Outcome::default()
            })
        }
        Some("order") => {
            if names.is_empty() {
                return Err(usage("baton account order <name...>"));
            }
            let order = order_accounts(&paths, &names, By::Name)?;
            Ok(Outcome {
                data: data(&order),
                screen: Some(Box::new(move |term| screens::account_order(term, &order.order))),
                ..Outcome::default()
            })
        }
        Some("move") => {
            let [name, position] = names.as_slice() else {
                return Err(usage("baton account move <name> <position>"));
            };
            let order = move_account(&paths, name, &Value::String((*position).to_owned()), By::Name)?;
            Ok(Outcome {
                data: data(&order),
                screen: Some(Box::new(move |term| screens::account_order(term, &order.order))),
                ..Outcome::default()
            })
        }
        Some("set") => {
            let Some(name) = names.first() else {
                return Err(usage("baton account set <name> ..."));
            };
            let changes = AccountChanges {
                mark: values.text("mark").map(str::to_owned),
                last_resort: bool_option(values.text("last-resort"), "last-resort")?,
                five_hour: percent_or_default(values.text("five-hour"), "five-hour")?,
                weekly: percent_or_default(values.text("weekly"), "weekly")?,
            };
            let changed = set_account(&paths, name, &changes, By::Name)?;
            Ok(Outcome {
                data: data(&changed),
                screen: Some(Box::new(move |term| screens::account_set(term, &changed.account))),
                ..Outcome::default()
            })
        }
        Some(other) => Err(usage(format!("Unknown: baton account {other}"))),
    }
}

/// The name of someone who takes part in the sync: an account's, or `~/.claude.json`.
fn party_name(config: &Config, who: &str) -> String {
    if who == HOME {
        "~/.claude.json".to_owned()
    } else {
        name_of(config, who)
    }
}

/// `baton sync`: bring every account's settings in step now, and show or answer what they differ on.
fn run_sync(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let paths = ctx.paths.clone();
    let now = (ctx.io.now)();
    let config = load_config(&paths);
    let failed = |error: std::io::Error| CliError::Failed(error.to_string());
    if !full_sync(&config) {
        return Ok(Outcome::shown(
            json!({ "on": false }),
            Box::new(move |term| {
                let name = |who: &str| party_name(&config, who);
                screens::sync_screen(
                    term,
                    &SyncView {
                        on: false,
                        report: None,
                        took: None,
                        name: &name,
                    },
                )
            }),
        ));
    }
    let mut took: Option<(usize, String)> = None;
    let report = match ctx.values.positional(0) {
        None => sync_settings(&paths, &config, Apply::All, now).map_err(failed)?,
        Some("take") => {
            let (Some(which), Some(whose)) = (ctx.values.positional(1), ctx.values.positional(2)) else {
                return Err(usage("baton sync take <number|all> <account|home>"));
            };
            let who = match whose {
                "home" | "~" | "~/.claude.json" => HOME.to_owned(),
                name => match find_account(&config, name, By::Name) {
                    Some(account) => account.id.clone(),
                    None => return Err(usage(format!("No account named {name}."))),
                },
            };
            // What changed since Baton last looked may be a question by now, or no longer one.
            let open = sync_settings(&paths, &config, Apply::Nothing, now)
                .map_err(failed)?
                .questions;
            let leaves: Vec<String> = if which == "all" {
                open.iter().map(|question| question.leaf.clone()).collect()
            } else {
                let at = which.parse::<usize>().ok().and_then(|n| n.checked_sub(1));
                match at.and_then(|at| open.get(at)) {
                    Some(question) => vec![question.leaf.clone()],
                    None if open.is_empty() => return Err(usage("The accounts differ on nothing.")),
                    None => {
                        return Err(usage(format!(
                            "baton sync take takes a number from 1 to {}, or all.",
                            open.len()
                        )));
                    }
                }
            };
            took = Some((leaves.len(), party_name(&config, &who)));
            take(&paths, &config, &leaves, &who, now).map_err(|error| usage(format!("{error}.")))?
        }
        Some(other) => return Err(usage(format!("Unknown: baton sync {other}"))),
    };
    let mut out = data(&report)
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    out.insert("on".to_owned(), json!(true));
    let code = if report.busy { 1 } else { 0 };
    Ok(Outcome::shown(
        Value::Object(out),
        Box::new(move |term| {
            let name = |who: &str| party_name(&config, who);
            let took = took.as_ref().map(|(count, who)| (*count, who.as_str()));
            screens::sync_screen(
                term,
                &SyncView {
                    on: true,
                    report: Some(&report),
                    took,
                    name: &name,
                },
            )
        }),
    )
    .code(code))
}

/// `baton login <account>`: Claude Code's own sign-in, under the account's profile.
fn run_login(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let Some(name) = ctx.values.positional(0) else {
        return Err(usage("baton login <account>"));
    };
    let paths = &ctx.paths;
    let env = &ctx.io.env;
    let config = load_config(paths);
    let Some(account) = find_account(&config, name, By::Name) else {
        return Err(usage(format!(
            "No account named {name}; add it with baton account add {}.",
            quoted(name)
        )));
    };
    let failed = |error: &dyn std::fmt::Display| CliError::Failed(error.to_string());
    link_profile(paths, &account.id, &LinkOptions::default()).map_err(|error| failed(&error))?;
    let launcher = resolve_claude(config.claude_path.as_deref(), env, Some(&paths.home))
        .map_err(|error| failed(&error))?;
    let code = sign_in(SignInOptions::new(
        launcher,
        profile_env(paths, &account.id, env),
        &account.name,
    ));
    if code == 0 {
        request_poll(paths, &account.id, "signed in", (ctx.io.now)());
    }
    Ok(Outcome::quiet(code))
}

// ---- Usage and policy ---------------------------------------------------------------------------

/// `baton view`: the menu.
fn run_view(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    if !ctx.io.out.tty || !ctx.io.stdin_tty {
        return Err(usage("The menu needs a terminal; try baton status."));
    }
    let options = MenuOptions {
        handle_signals: true,
        ..MenuOptions::new(ctx.paths.clone(), ctx.io.env.clone())
    };
    match run_menu(options) {
        Ok(code) => Ok(Outcome::quiet(code)),
        Err(error) => Err(CliError::Failed(error.to_string())),
    }
}

/// `baton usage`: every account's plan usage; `--refresh` asks Claude Code for it now.
fn run_usage(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let paths = ctx.paths.clone();
    let now = (ctx.io.now)();
    let config = load_config(&paths);
    let id = match ctx.values.positional(0) {
        Some(name) => match find_account(&config, name, By::Name) {
            Some(account) => Some(account.id.clone()),
            None => return Err(usage(format!("No account named {name}."))),
        },
        None => None,
    };
    if ctx.values.flag("refresh") {
        // In this process, which exits when it is done.
        let poller = Poller::new(PollerOptions::new(paths, ctx.io.env.clone()));
        let results = poller.refresh_wait(id.as_deref(), "cli");
        return Ok(Outcome::shown(
            json!({ "results": results }),
            Box::new(move |term| screens::refresh_screen(term, &results, now)),
        ));
    }
    let view = status_view(&load_snapshot(
        &paths,
        now,
        Want {
            sessions: false,
            poll: true,
        },
    ));
    let accounts: Vec<Value> = view
        .at("accounts")
        .as_array()
        .map(|accounts| {
            accounts
                .iter()
                .filter(|a| id.as_deref().is_none_or(|id| a.text("id") == Some(id)))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Ok(Outcome::shown(
        json!({ "accounts": accounts, "poll": view.at("poll") }),
        Box::new(move |term| screens::usage_screen(term, load_menu_data(&paths, now), id.as_deref())),
    ))
}

/// `baton status`: every account at a glance, and where new work goes.
fn run_status(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let paths = ctx.paths.clone();
    let now = (ctx.io.now)();
    let view = status_view(&load_snapshot(
        &paths,
        now,
        Want {
            sessions: true,
            poll: true,
        },
    ));
    Ok(Outcome::shown(
        view,
        Box::new(move |term| screens::status_screen(term, load_menu_data(&paths, now))),
    ))
}

/// `baton pick`: the account new work would go to now.
fn run_pick(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let now = (ctx.io.now)();
    let snapshot = load_snapshot(&ctx.paths, now, Want::default());
    if snapshot.states.is_empty() {
        return Ok(Outcome {
            data: Some(json!({ "account": null, "accounts": 0 })),
            pipe: Some(NO_ACCOUNTS.to_owned()),
            screen: Some(Box::new(screens::empty_screen)),
            code: 3,
            ..Outcome::default()
        });
    }
    let pick = pick_start(&snapshot.states, &snapshot.policy, &[]);
    let at = pick.available_at.filter(|at| pick.available_known && *at != 0);
    let label = |id: &str| {
        snapshot
            .states
            .iter()
            .find(|s| s.id == id)
            .map_or_else(|| id.to_owned(), |s| s.name.clone())
    };
    let name = pick.account.as_deref().map(label);
    let earliest = pick.earliest.as_deref().map(label);
    let reason = pick.reason;
    Ok(Outcome {
        data: Some(json!({
            "account": pick.account,
            "reason": reason,
            "allBlocked": pick.all_blocked,
            "earliest": pick.earliest,
            "availableAt": at.map(iso),
        })),
        // Piped, the bare name: `account=$(baton pick)`.
        pipe: Some(match &pick.account {
            Some(account) => account.clone(),
            None => format!(
                "none (all blocked; first back: {})",
                pick.earliest.as_deref().unwrap_or("unknown")
            ),
        }),
        code: if pick.account.is_some() { 0 } else { 3 },
        screen: Some(Box::new(move |term| {
            let at = if pick.available_known {
                pick.available_at
            } else {
                None
            };
            screens::pick_screen(term, name.as_deref(), reason, earliest.as_deref(), at, now)
        })),
        ..Outcome::default()
    })
}

/// `baton stats`: the last days of usage, moves and refusals.
fn run_stats(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let days = match ctx.values.text("days") {
        None => 7.0,
        Some(days) => loose_f64(&Value::String(days.to_owned())),
    };
    if days.fract() != 0.0 || !(1.0..=90.0).contains(&days) {
        return Err(usage("--days takes 1 to 90."));
    }
    let paths = ctx.paths.clone();
    let now = (ctx.io.now)();
    let config = load_config(&paths);
    let stats = compute_stats(&paths, &config, &effective_policy(&config), now, days as i64);
    Ok(Outcome {
        data: data(&stats),
        screen: Some(Box::new(move |term| {
            screens::stats_screen(term, load_menu_data(&paths, now), stats)
        })),
        ..Outcome::default()
    })
}

/// The flags of `baton policy` and the settings they change, in the order they are shown.
const POLICY_FLAGS: [(&str, &str); 25] = [
    ("mode", "mode"),
    ("five-hour", "fiveHour"),
    ("weekly", "weekly"),
    ("start-reserve", "startReserve"),
    ("plan-factor", "planFactor"),
    ("cost-samples", "costSamples"),
    ("min-stay", "minStayMinutes"),
    ("wait-in-place", "waitInPlaceMinutes"),
    ("wait-in-place-max", "waitInPlaceMaxHours"),
    ("rebalance-horizon", "rebalanceHorizonMinutes"),
    ("rebalance-margin", "rebalanceMargin"),
    ("burn-window", "burnWindowMinutes"),
    ("unknown-rejection", "unknownRejectionHours"),
    ("countdown", "countdownSeconds"),
    ("max-reblocks", "maxReblocks"),
    ("reblock-backoff", "reblockBackoffMinutes"),
    ("settle-ms", "settleMs"),
    ("compaction-rebalance", "compactionRebalance"),
    ("status-line-usage", "statusLineUsage"),
    ("usage-poll", "usagePollMinutes"),
    ("full-sync", "fullSync"),
    ("history-days", "historyDays"),
    ("session-days", "sessionDays"),
    ("log-days", "logDays"),
    ("backups-kept", "backupsKept"),
];

/// A setting's value as the screen shows it.
fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), show),
        other => other.to_string(),
    }
}

/// `baton policy`: show or change the policy settings.
fn run_policy(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let mut changes = Object::new();
    for (flag, key) in POLICY_FLAGS {
        if let Some(value) = ctx.values.text(flag) {
            let value = if value.eq_ignore_ascii_case("default") {
                Value::Null
            } else {
                json!(value)
            };
            changes.insert(key.to_owned(), value);
        }
    }
    let saved = changes.len();
    if saved > 0 {
        set_policy(&ctx.paths, &changes)?;
    }
    let config = load_config(&ctx.paths);
    let policy = effective_policy(&config);
    let defaults = Policy::default();
    let rows: Vec<PolicyRow> = POLICY_FLAGS
        .iter()
        .map(|(flag, key)| PolicyRow {
            flag: (*flag).to_owned(),
            value: policy.get(key).as_ref().map(shown).unwrap_or_default(),
            custom: config
                .policy
                .get(*key)
                .is_some_and(|value| Some(value) != defaults.get(key).as_ref()),
        })
        .collect();
    let mode = policy.mode.as_str();
    Ok(Outcome::shown(
        Value::Object(policy.to_json()),
        Box::new(move |term| screens::policy_screen(term, &rows, mode, saved)),
    ))
}

// ---- Setup --------------------------------------------------------------------------------------

/// Links every account's profile again; what cannot be linked is doctor's to report.
fn refresh_profiles(paths: &Paths) {
    for account in load_config(paths).accounts {
        let _ = link_profile(paths, &account.id, &LinkOptions::default());
    }
}

/// `baton install` and `baton uninstall`: plan, show, and unless `--dry-run`, apply.
fn change_settings(mode: &'static str, ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let paths = ctx.paths.clone();
    let env = &ctx.io.env;
    let ferry = ctx.io.ferry();
    let planned = if mode == "install" {
        plan_install(&paths, &ctx.io.exe, env, ferry)
    } else {
        plan_uninstall(&paths, env, ferry)
    };
    let mut plan = planned.map_err(|error| usage(error.0))?;
    let dry_run = ctx.values.flag("dry-run");
    let mut applied = None;
    if !dry_run {
        let done = apply_plan(&mut plan, &paths, (ctx.io.now)(), env, ferry.run);
        applied = Some(done.map_err(|error| usage(error.0))?);
        refresh_profiles(&paths);
    }
    let mut out = Object::new();
    out.insert("dryRun".to_owned(), json!(dry_run));
    let files: Vec<Value> = plan
        .files
        .iter()
        .map(|file| json!({ "path": text_of(&file.path), "diff": file.diff }))
        .collect();
    out.insert("files".to_owned(), Value::Array(files));
    out.insert("notes".to_owned(), json!(plan.notes));
    if let Some(applied) = &applied {
        out.insert("changed".to_owned(), json!(applied.changed));
        out.insert("backup".to_owned(), json!(applied.backup.as_deref().map(text_of)));
    }
    let diff = ctx.values.flag("diff");
    let has_accounts = !load_config(&paths).accounts.is_empty();
    let home = text_of(&paths.home);
    Ok(Outcome::shown(
        Value::Object(out),
        Box::new(move |term| {
            let view = PlanView {
                mode,
                plan: &plan,
                dry_run,
                applied: applied.as_ref(),
                diff,
                home: &home,
                has_accounts,
            };
            screens::plan_screen(term, &view)
        }),
    ))
}

/// `baton install`.
fn run_install(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    change_settings("install", ctx)
}

/// `baton uninstall`.
fn run_uninstall(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    change_settings("uninstall", ctx)
}

/// `baton doctor`: check Claude Code, the profiles, the install and Ferry.
fn run_doctor_command(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let report = run_doctor(&DoctorOptions {
        paths: &ctx.paths,
        env: &ctx.io.env,
        exe: &ctx.io.exe,
        auth: true,
        ferry: ctx.io.ferry(),
        now: (ctx.io.now)(),
    });
    let code = if report.ok { 0 } else { 1 };
    Ok(Outcome::shown(
        report.to_json(),
        Box::new(move |term| screens::doctor_screen(term, &report.checks)),
    )
    .code(code))
}

/// `baton conflicts`: shared files changed in two places at once.
fn run_conflicts(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let list = read_conflicts(&ctx.paths);
    let config = load_config(&ctx.paths);
    let label = move |id: &str| name_of(&config, id);
    if ctx.values.flag("clear") {
        clear_conflicts(&ctx.paths).map_err(|error| CliError::Failed(error.to_string()))?;
        let cleared = list.len();
        return Ok(Outcome::shown(
            json!({ "cleared": cleared }),
            Box::new(move |term| screens::conflicts_screen(term, &list, &label, Some(cleared))),
        ));
    }
    Ok(Outcome {
        data: data(&list),
        screen: Some(Box::new(move |term| {
            screens::conflicts_screen(term, &list, &label, None)
        })),
        ..Outcome::default()
    })
}

/// `baton version`.
fn run_version(_ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    let version = env!("CARGO_PKG_VERSION");
    Ok(Outcome {
        data: Some(json!({ "version": version })),
        text: Some(version.to_owned()),
        ..Outcome::default()
    })
}

/// `baton help [command]`.
fn run_help(ctx: &mut Ctx<'_, '_>) -> Result<Outcome, CliError> {
    Ok(help_outcome(ctx.values.positional(0)))
}

/// Every command, in the order they are listed.
pub fn commands() -> Vec<Command> {
    vec![
        command(
            "hook",
            "internal",
            &["baton hook <session-start|prompt|stop|stop-failure|session-end>"],
            "Record a Claude Code hook event (Claude Code runs this; reads the hook JSON on stdin).",
            run_hook,
        ),
        command(
            "statusline",
            "internal",
            &["baton statusline"],
            "Record rate_limits and print the status line (Claude Code runs this).",
            run_statusline,
        ),
        Command {
            example: Some("baton --model opus --add-dir ../shared"),
            raw: true,
            ..command(
                "start",
                "session",
                &["baton [claude arguments...]", "baton start [claude arguments...]"],
                "Start Claude Code on the best account, with the terminal attached; hop the conversation when an account runs out. baton alone does it; baton start also takes a prompt first (baton start \"fix the tests\").",
                run_start,
            )
        },
        Command {
            example: Some("baton --model opus --add-dir ../shared"),
            raw: true,
            alias: true,
            ..command(
                "claude",
                "session",
                &["baton claude [claude arguments...]"],
                "Same as baton.",
                run_start,
            )
        },
        Command {
            options: vec![
                Spec::text("to"),
                Spec::text("session"),
                Spec::flag("from-command"),
            ],
            ..command(
                "hop",
                "session",
                &["baton hop [session] [--to account]"],
                "Move a conversation running under baton to another account at the end of its turn.",
                run_hop,
            )
        },
        Command {
            options: vec![
                Spec::text("mark"),
                Spec::text("last-resort"),
                Spec::text("position"),
                Spec::text("five-hour"),
                Spec::text("weekly"),
            ],
            prepare: Some(prepare_account),
            ..command(
                "account",
                "accounts",
                &[
                    "baton account list",
                    "baton account add <name> [--mark m] [--last-resort] [--position n]",
                    "baton account rename <name> <new name>",
                    "baton account remove <name>",
                    "baton account order <name...>",
                    "baton account move <name> <position>",
                    "baton account set <name> [--mark preferred|normal|last-resort] [--last-resort true|false] [--five-hour n|default] [--weekly n|default]",
                ],
                "Manage the accounts: names, order, marks and thresholds.",
                run_account,
            )
        },
        Command {
            usage: vec![
                "baton sync [--json]".to_owned(),
                "baton sync take <number|all> <account|home>".to_owned(),
            ],
            ..command(
                "sync",
                "accounts",
                &[],
                "Keep what you set up (preferences, MCP servers, each folder's trust and approvals) the same on every account: bring every account in step now, show what the accounts differ on, and say whose value stays (take).",
                run_sync,
            )
        },
        command(
            "login",
            "accounts",
            &["baton login <account>"],
            "Sign an account in: runs `claude auth login` under its profile with the terminal attached.",
            run_login,
        ),
        command(
            "view",
            "usage",
            &["baton view"],
            "The menu: accounts, sessions, policy and stats, live.",
            run_view,
        ),
        Command {
            alias: true,
            ..command("menu", "usage", &["baton menu"], "Same as baton view.", run_view)
        },
        Command {
            options: vec![Spec::flag("refresh")],
            ..command(
                "usage",
                "usage",
                &["baton usage [--refresh] [account] [--json]"],
                "Every account's plan usage, where each reading came from and how old it is, and the usage poll; --refresh asks Claude Code for it now (one account or all).",
                run_usage,
            )
        },
        command(
            "status",
            "usage",
            &["baton status [--json]"],
            "Accounts in order, their 5-hour and weekly use, blocks and resets, sessions, where new work goes, and the usage poll.",
            run_status,
        ),
        command(
            "pick",
            "usage",
            &["baton pick [--json]"],
            "The account new work would go to now.",
            run_pick,
        ),
        Command {
            options: vec![Spec::text("days")],
            ..command(
                "stats",
                "usage",
                &["baton stats [--days n] [--json]"],
                "Usage per account over the last days, hops and refusals per day, weekly allowance left unused at resets, sessions.",
                run_stats,
            )
        },
        Command {
            options: POLICY_FLAGS.iter().map(|(flag, _)| Spec::text(flag)).collect(),
            usage: vec![format!(
                "baton policy [--json] [{}]",
                POLICY_FLAGS
                    .iter()
                    .map(|(flag, _)| format!("--{flag} <value|default>"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )],
            ..command(
                "policy",
                "usage",
                &[],
                "Show or change the policy settings (smart or order mode, thresholds, reserves, timings).",
                run_policy,
            )
        },
        Command {
            options: vec![Spec::flag("dry-run"), Spec::flag("diff")],
            ..command(
                "install",
                "setup",
                &["baton install [--dry-run] [--diff]"],
                "Add Baton's status line, hooks and /hop command to ~/.claude, and register Baton with Ferry when it is installed, as the agent it opens (shows the changes, keeps a backup, idempotent); --diff lists every changed line.",
                run_install,
            )
        },
        Command {
            options: vec![Spec::flag("dry-run"), Spec::flag("diff")],
            ..command(
                "uninstall",
                "setup",
                &["baton uninstall [--dry-run] [--diff]"],
                "Remove Baton's status line, hooks and /hop command from ~/.claude, restore the previous status line, and remove Baton's Ferry launcher, putting back the agent Ferry opened before; --diff lists every changed line.",
                run_uninstall,
            )
        },
        command(
            "doctor",
            "setup",
            &["baton doctor [--json]"],
            "Check Claude Code, every profile's links and sign-in, the installed status line and hooks, the Ferry registration and the agent Ferry opens.",
            run_doctor_command,
        ),
        Command {
            options: vec![Spec::flag("clear")],
            ..command(
                "conflicts",
                "setup",
                &["baton conflicts [--clear] [--json]"],
                "Shared files changed both in a profile and in ~/.claude (without file links), with the backups kept.",
                run_conflicts,
            )
        },
        command(
            "version",
            "setup",
            &["baton version"],
            "Print Baton's version.",
            run_version,
        ),
        command(
            "help",
            "setup",
            &["baton help [command]"],
            "Show the commands, or one command's usage.",
            run_help,
        ),
    ]
}
