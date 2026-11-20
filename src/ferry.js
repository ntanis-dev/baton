import { spawnSync } from "node:child_process";
import { statSync } from "node:fs";
import { join, resolve } from "node:path";
import { findOnPath } from "./claude-path.js";
import { readText } from "./fsutil.js";
import { forward, samePath } from "./paths.js";

// Baton as a Ferry launcher. When Ferry is on this machine (its
// data folder exists, or `ferry` is on PATH), `baton install` writes
// <data>/launchers/baton.json, so Ferry can open a project in Baton and
// resume or continue a Claude Code conversation there; `baton uninstall`
// removes the file while it is still Baton's. <data> is FERRY_HOME, else
// ~/.ferry in Baton's own home (BATON_HOME keeps the tests away from the
// real one). The file runs node + Baton's cli.js directly (exec form, like
// the hooks): no shell and no .cmd shim. Its format is Ferry's launcher
// schema, version 1.
//
// The file names Claude Code's own option for starting without asking for
// approval (bypassArgs), so Ferry's bypass-permissions setting and the Bypass
// of a running session cover Baton as they do Claude Code. The setting stays
// Ferry's and the operator's: Baton names the option and turns nothing on.
// Ferry puts the option first, and Baton hands it to claude with the rest.

export const LAUNCHER_ID = "baton";
export const LAUNCHER_SOURCE = "baton install";

const isDir = (p) => {
  try {
    return statSync(p).isDirectory();
  } catch {
    return false;
  }
};

export const ferryHome = (paths, env = process.env) =>
  env.FERRY_HOME ? resolve(env.FERRY_HOME) : join(paths.home, ".ferry");

export const launcherPath = (data) =>
  join(data, "launchers", `${LAUNCHER_ID}.json`);

// Ferry keeps its data folder private: a folder Baton creates there is
// this user's only, and so is the launcher file, as Ferry writes its own.
export const PRIVATE = { dirMode: 0o700, mode: 0o600 };

// { found, data, file, program, cli }: cli is where `ferry` is on PATH (null
// when it is not), and program the same, but only when the data folder does
// not exist (then it is what says Ferry is here). findFerry() -> path | null
// replaces the PATH lookup (the tests pass their own).
export function detectFerry({
  paths,
  env = process.env,
  platform = process.platform,
  findFerry = () => findOnPath("ferry", { env, platform }),
}) {
  const data = ferryHome(paths, env);
  const cli = findFerry() ?? null;
  const program = isDir(data) ? null : cli;
  return {
    found: program !== null || isDir(data),
    data,
    file: launcherPath(data),
    program,
    cli,
  };
}

// ---- The agent Ferry opens -------------------------------------------------
// Ferry's `c` key and `ferry agent` open one agent launcher: its "agent"
// setting. Baton takes that place while it is registered, through Ferry's
// own CLI (`ferry config agent baton`), so the setting stays Ferry's to
// check and keep; `baton uninstall` puts the one before back. A launcher
// chosen on purpose (anything but Ferry's default or its built-in Claude
// Code) stays. runFerry(program, args, env) -> { status, stdout, stderr }
// runs Ferry (the tests pass their own).

// Ferry's built-in Claude Code launcher, which Baton stands in for.
export const CLAUDE_LAUNCHER = "claude";

// Claude Code's option that starts it without asking for approval: what Ferry
// adds for a launcher its bypass-permissions setting names.
export const BYPASS_ARG = "--dangerously-skip-permissions";

export function runFerrySync(program, args, env = process.env) {
  const result = spawnSync(program, args, {
    env,
    encoding: "utf8",
    timeout: 20e3,
    windowsHide: true,
  });
  return {
    status: result.error ? null : result.status,
    stdout: result.stdout ?? "",
    stderr: (result.error?.message ?? result.stderr ?? "").toString(),
  };
}

// What `c` opens: { value, default } from `ferry config agent --json` (value
// is a launcher id, or "default" for the first installed), or null when
// Ferry did not say.
export function ferryAgent(
  program,
  { env = process.env, runFerry = runFerrySync } = {},
) {
  const result = runFerry(program, ["config", "agent", "--json"], env);
  if (result.status !== 0) return null;
  try {
    const value = JSON.parse(result.stdout);
    if (value && typeof value.value === "string")
      return { value: value.value, default: Boolean(value.default) };
  } catch {
    // not JSON
  }
  return null;
}

// Whether Baton takes the agent's place: Ferry's default, or its built-in
// Claude Code.
export const takesOver = (agent) =>
  agent.default || agent.value === CLAUDE_LAUNCHER;

export const setFerryAgent = (
  program,
  id,
  { env = process.env, runFerry = runFerrySync } = {},
) => runFerry(program, ["config", "agent", id], env);

// Braces are Ferry's placeholders: a path's own are doubled.
const literal = (text) => text.replace(/[{}]/g, "$&$&");

export function launcherDef(nodePath, cliPath) {
  return {
    schemaVersion: 1,
    id: LAUNCHER_ID,
    name: "Baton",
    description: "Claude Code on your accounts, one after another",
    program: forward(nodePath),
    args: [literal(forward(cliPath))],
    resumeArgs: ["--resume", "{conversation}"],
    continueArgs: ["--continue"],
    bypassArgs: [BYPASS_ARG],
    conversations: "claude-code",
    terminal: true,
    source: LAUNCHER_SOURCE,
  };
}

export const launcherText = (nodePath, cliPath) =>
  JSON.stringify(launcherDef(nodePath, cliPath), null, 2) + "\n";

export function parseLauncher(text) {
  try {
    const value = JSON.parse(text);
    return value && typeof value === "object" && !Array.isArray(value)
      ? value
      : null;
  } catch {
    return null;
  }
}

// Only a file Baton wrote is Baton's to remove.
export const isBatonLauncher = (value) =>
  value?.id === LAUNCHER_ID && value?.source === LAUNCHER_SOURCE;

// The registration, for doctor: state is absent (no Ferry), missing,
// current, stale (Baton's, but not what baton install writes now: node and
// cli say which path changed) or foreign (a baton.json that is not Baton's).
export function registrationState({
  paths,
  env = process.env,
  platform = process.platform,
  findFerry,
  nodePath = process.execPath,
  cliPath,
}) {
  const ferry = detectFerry({ paths, env, platform, findFerry });
  if (!ferry.found) return { ...ferry, state: "absent" };
  const text = readText(ferry.file);
  if (text === null) return { ...ferry, state: "missing" };
  if (text === launcherText(nodePath, cliPath))
    return { ...ferry, state: "current" };
  const value = parseLauncher(text);
  if (!isBatonLauncher(value)) return { ...ferry, state: "foreign" };
  const want = launcherDef(nodePath, cliPath);
  return {
    ...ferry,
    state: "stale",
    node: !samePath(String(value.program), want.program, platform),
    cli: !samePath(
      String(Array.isArray(value.args) ? value.args[0] : ""),
      want.args[0],
      platform,
    ),
  };
}
