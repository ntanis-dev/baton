# Smoke test with the real Claude Code

The test suite runs a stand-in for `claude`, so it cannot notice when the real one changes how it starts, ends,
reports its usage or saves its files. This list is gone through by hand before a release, after a new version of
Claude Code, and after a change to how Baton starts, ends or follows `claude` (the wrapper, the hooks, the usage
check, the shared files). It takes about twenty minutes and two signed-in accounts.

Everything happens in a scratch home, so your own accounts and `~/.claude` are not touched.

## Setup

- [ ] Build what will be released: `cargo build --release -p baton`. Use `target/release/baton` below.
- [ ] Point `BATON_HOME` at a new, empty folder, in every terminal used below. `~/.baton`, `~/.claude` and
      `~/.claude.json` are then the ones under that folder.
- [ ] `claude` is on `PATH` (under `BATON_HOME` the usual install places are not looked at), or `BATON_CLAUDE` names
      it. Note its version: `claude --version`.

## Accounts and install

- [ ] `baton account add "One"`, `baton login "One"`, then the same for `"Two" --last-resort`. Each sign-in is
      Claude Code's own, and stays in the scratch profile.
- [ ] `baton install --dry-run --diff` lists the status line, six hooks and `/hop`; nothing is written yet.
- [ ] `baton install` writes them; a second `baton install` changes nothing.
- [ ] `baton doctor` has no failed check, and a usage check line for each account.
- [ ] `baton usage --refresh` gets a reading for both accounts.

## A conversation

- [ ] `baton` starts Claude Code on One. The status line shows One and its windows after the first answer.
- [ ] In a second terminal, `baton status` shows One's reading as from the session, and the session as live.
- [ ] `/hop` inside Claude Code: at the end of the turn Baton says why, Claude Code ends, and the same conversation
      resumes on Two. Ask it what was said before: it knows.
- [ ] `baton hop --to One` from the second terminal moves it back at the end of the next turn.
- [ ] The model was never told an account's name: the first prompt after a move names none.

## Limits

- [ ] `baton account set One --five-hour 1` puts One over its threshold. At the end of the next turn Baton counts
      down to a move: Enter moves now, any other key stays, Ctrl+C quits. Try each.
- [ ] With both accounts at `--five-hour 1`: Baton says every account is at its limit and which is first back; any
      key stays.
- [ ] `baton account set One --five-hour default` (and Two) puts them back.

## Two at once

- [ ] While the conversation runs, resume the same one from a second terminal. The second says the first holds it,
      and neither moves the other's conversation.
- [ ] `baton view` in a third terminal while both run: one of them checks the usage, never two at once.

## Ending

- [ ] Leave Claude Code with `/exit`, and another time with Ctrl+C twice: Baton ends with it, and the terminal is as
      it was (the cursor shows, typing echoes, the mouse selects text).
- [ ] Close the terminal window in the middle of a turn: no `claude` and no process it started is left running.
- [ ] During a countdown, keys typed earlier do not answer it.

## One setup, every account

- [ ] Change a setting in Claude Code on One (`/config`, the theme). On Two it is the same at its next start, and
      `baton sync` says the accounts are in step.
- [ ] Give the two accounts different values of one setting while neither runs: `baton sync` lists it as a question,
      and `baton sync take 1 One` settles it.
- [ ] Approve a tool in a folder on One: Two has the approval too, and taking it away on one account does not take
      it from the other.
- [ ] Edit `CLAUDE.md` on both accounts, in different lines: both edits are kept.

## The menu

- [ ] `baton view` in a window of 80 x 24 and in a large one: every view draws, and resizing redraws it.
- [ ] `?` lists the keys; `r` asks the usage; a number typed into a setting of the Policy form shows and is applied
      after the confirmation.
- [ ] Sign an account in from its Actions: the address is written out in full, and the menu comes back after.
- [ ] Click a row, scroll with the wheel, quit with `q`: the terminal is as it was.
- [ ] In a terminal with a light background, the printed commands (`baton status`, `baton doctor`) are readable.

## Each system

- [ ] Windows: Windows Terminal, and the classic console (plain glyphs, nothing drawn as a box).
- [ ] macOS: Terminal.app (the status line's colours are right) and one other terminal.
- [ ] Linux: a desktop terminal; the virtual console if one is at hand (plain glyphs).

## Leaving

- [ ] `baton uninstall --dry-run --diff`, then `baton uninstall`: the scratch `~/.claude/settings.json` is what it was
      before `baton install`.
- [ ] Delete the scratch folder.

## The real home

Only when the release goes on your own machine: install the release build, read `baton install --dry-run --diff`
before `baton install`, then `baton doctor`. Sessions that run keep the build they started with until they end.
