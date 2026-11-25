//! The few things Baton asks of the operating system that std does not offer: whether a process is
//! alive, a lower priority for a background child, (unix) a polite request to end, and letting the
//! console's interrupts pass this process by.
//!
//! This is the only module of the workspace with `unsafe`: each block is one system call on a process id
//! or a handle this module opened itself and closes again.
#![allow(unsafe_code)]

/// Whether process `pid` is alive. A process that is there but belongs to someone else counts as alive.
pub fn is_alive(pid: i64) -> bool {
    if pid <= 0 || pid > i64::from(u32::MAX) {
        return false;
    }
    alive(pid as u32)
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: OpenProcess takes plain values; the handle it returns is checked, used for one query and
    // closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        let asked = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        asked != 0 && code == STILL_ACTIVE as u32
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists; nothing is sent.
    let sent = unsafe { libc::kill(pid, 0) };
    sent == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Runs process `pid` at a low priority (Windows: below normal), so that work in the background never
/// competes with the session in front of the operator. Best effort: a process that is gone, or that may
/// not be changed, keeps its priority.
pub fn lower_priority(pid: u32) {
    if pid == 0 {
        return;
    }
    lower(pid);
}

#[cfg(windows)]
fn lower(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        BELOW_NORMAL_PRIORITY_CLASS, OpenProcess, PROCESS_SET_INFORMATION, SetPriorityClass,
    };
    // SAFETY: the handle is checked, used for one call and closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_SET_INFORMATION, 0, pid);
        if handle.is_null() {
            return;
        }
        SetPriorityClass(handle, BELOW_NORMAL_PRIORITY_CLASS);
        CloseHandle(handle);
    }
}

#[cfg(unix)]
fn lower(pid: u32) {
    // SAFETY: setpriority takes plain values and changes only the niceness of that process.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, 10);
    }
}

/// Asks process `pid` to end the way a terminal would (unix: SIGTERM). Returns whether the request was
/// delivered. Windows has no such request for one process: always false there.
pub fn terminate(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        // SAFETY: kill takes plain values; the pid is a positive process id, never a group.
        unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Makes this process ignore the console's interrupts (Ctrl+C and Ctrl+Break; on unix SIGINT and
/// SIGQUIT), or stop ignoring them. Only this process: a child started meanwhile still gets them, which
/// is the point, since they belong to the claude that the session wrapper runs.
pub fn ignore_interrupts(on: bool) {
    interrupts(on);
}

/// A console control handler that takes Ctrl+C and Ctrl+Break and does nothing with them.
#[cfg(windows)]
unsafe extern "system" fn swallow(kind: u32) -> windows_sys::core::BOOL {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    i32::from(kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT)
}

#[cfg(windows)]
fn interrupts(on: bool) {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: adds or removes a handler of this process that only returns a value. Not the form with no
    // handler, which sets a flag that child processes inherit.
    unsafe {
        SetConsoleCtrlHandler(Some(swallow), i32::from(on));
    }
}

/// A signal handler that does nothing.
#[cfg(unix)]
extern "C" fn nothing(_signal: libc::c_int) {}

#[cfg(unix)]
fn interrupts(on: bool) {
    let handler = if on {
        nothing as extern "C" fn(libc::c_int) as libc::sighandler_t
    } else {
        libc::SIG_DFL
    };
    // SAFETY: installs a handler that does nothing, or the default one. Not SIG_IGN, which a program
    // this process starts would keep.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGQUIT, handler);
    }
}

/// The exit code of a request to end this process that was noted and not yet acted on; 0 for none.
static END_REQUEST: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Whether what a request to end this process asked for is done ([`end_handled`]).
static END_HANDLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// From now on a request to end this process (on Windows Ctrl+Break, and the console being closed,
/// a logoff or a shutdown; SIGTERM and SIGHUP elsewhere) is noted for [`end_requested`] instead of
/// ending it at once, so that a full-screen program can give the terminal back first, and a session
/// wrapper can end its claude and let go of what it holds.
pub fn watch_end_requests() {
    watch_ends();
}

/// The exit code of a request to end this process that arrived since [`watch_end_requests`]: 149 for
/// Ctrl+Break, 143 for SIGTERM, 129 for SIGHUP and for a console that is closed. None when nobody
/// asked.
pub fn end_requested() -> Option<i32> {
    let code = END_REQUEST.load(std::sync::atomic::Ordering::SeqCst);
    (code != 0).then_some(code)
}

/// Says that what a request to end asked for is done. Windows ends a process whose console is closed
/// as soon as its handler returns, and the handler waits for this (a few seconds at most).
pub fn end_handled() {
    END_HANDLED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Notes a request to end as the system would, or takes one back (0): for whoever has a request of
/// its own to pass on, and for tests.
pub fn note_end_request(code: i32) {
    END_HANDLED.store(false, std::sync::atomic::Ordering::SeqCst);
    END_REQUEST.store(code, std::sync::atomic::Ordering::SeqCst);
}

/// A console control handler that notes Ctrl+Break, the console being closed, a logoff and a shutdown,
/// and leaves everything else to the next handler. For the last three the process is ended once this
/// returns, so it waits until the request is said to be handled: at most four and a half of the five
/// seconds Windows gives.
#[cfg(windows)]
unsafe extern "system" fn note_break(kind: u32) -> windows_sys::core::BOOL {
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };
    match kind {
        CTRL_BREAK_EVENT => {
            END_REQUEST.store(149, Ordering::SeqCst);
            1
        }
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            END_REQUEST.store(129, Ordering::SeqCst);
            for _ in 0..90 {
                if END_HANDLED.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            1
        }
        _ => 0,
    }
}

#[cfg(windows)]
fn watch_ends() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: adds a handler of this process that only stores a number.
    unsafe {
        SetConsoleCtrlHandler(Some(note_break), 1);
    }
}

/// A signal handler that notes the signal as an exit code: storing a number is all a handler may do.
#[cfg(unix)]
extern "C" fn note_signal(signal: libc::c_int) {
    END_REQUEST.store(128 + signal, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(unix)]
fn watch_ends() {
    let handler = note_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: installs a handler that only stores a number.
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

/// Types keys (their character codes) into the console this process is on, as the operator's keyboard
/// would: a full-screen program that reads the console raw (Claude Code) takes them for keys, and
/// Ctrl+C typed this way is a key and no signal. `delay_ms` is the pause between two keys. Answers
/// `ok`, or the step that failed with the system's error number. Only Windows has such a console.
pub fn type_console_keys(codes: &[u32], delay_ms: u64) -> String {
    match type_keys(codes, delay_ms) {
        Ok(()) => "ok".to_owned(),
        Err(step) => step,
    }
}

#[cfg(windows)]
fn type_keys(codes: &[u32], delay_ms: u64) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{
        INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, LEFT_CTRL_PRESSED,
        WriteConsoleInputW,
    };
    let failed = |step: &str, error: &std::io::Error| format!("{step} {}", error.raw_os_error().unwrap_or(0));
    let console = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .map_err(|error| failed("open", &error))?;
    for (at, code) in codes.iter().enumerate() {
        if at > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        let unit = u16::try_from(*code).map_err(|_| format!("key {code}"))?;
        // A control character is its letter with Ctrl held: 3 is Ctrl+C.
        let control = unit < 0x20;
        let key = if control {
            unit + 0x40
        } else {
            char::from_u32(*code).map_or(unit, |c| c.to_ascii_uppercase() as u16)
        };
        let record = |down: bool| INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: i32::from(down),
                    wRepeatCount: 1,
                    wVirtualKeyCode: key,
                    wVirtualScanCode: 0,
                    uChar: KEY_EVENT_RECORD_0 { UnicodeChar: unit },
                    dwControlKeyState: if control { LEFT_CTRL_PRESSED } else { 0 },
                },
            },
        };
        let records = [record(true), record(false)];
        let mut written = 0u32;
        // SAFETY: the handle is this function's own open console input, and the buffer is two records
        // that live until the call returns.
        let wrote = unsafe {
            WriteConsoleInputW(
                console.as_raw_handle().cast(),
                records.as_ptr(),
                records.len() as u32,
                &mut written,
            )
        };
        if wrote == 0 {
            return Err(failed("write", &std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn type_keys(_codes: &[u32], _delay_ms: u64) -> Result<(), String> {
    Err("no console".to_owned())
}

/// A child process with everything it started, so that all of it can be ended together: what a
/// forced end of the child alone would leave running (Claude Code's MCP servers and shells).
#[derive(Debug)]
pub struct ProcessTree {
    /// The child.
    #[cfg(unix)]
    pid: u32,
    /// Whether the child leads a process group of its own ([`own_group`]).
    #[cfg(unix)]
    grouped: bool,
    /// The job the child was put in; 0 when that could not be done.
    #[cfg(windows)]
    job: usize,
}

/// Starts `command` in a process group of its own (unix), so that [`ProcessTree::background`] can end
/// it with everything it starts. Not for a program that needs the terminal: the terminal belongs to
/// the group in front.
pub fn own_group(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

impl ProcessTree {
    /// The tree of a child that works in the background, started with [`own_group`]. On Windows it is
    /// ended with this value at the latest, and so with this process however it ends.
    pub fn background(pid: u32) -> ProcessTree {
        ProcessTree::of(pid, true)
    }

    /// The tree of the program in front of the operator. It ends only when [`ProcessTree::kill`]
    /// says so.
    pub fn foreground(pid: u32) -> ProcessTree {
        ProcessTree::of(pid, false)
    }

    /// The tree of `pid`; `background` says which kind.
    fn of(pid: u32, background: bool) -> ProcessTree {
        #[cfg(windows)]
        {
            ProcessTree {
                job: job_of(pid, background),
            }
        }
        #[cfg(unix)]
        {
            ProcessTree {
                pid,
                grouped: background,
            }
        }
    }

    /// Ends the child and everything it started, at once. Whether the system took the request for
    /// the whole tree (false: only what could be found of it was ended, or nothing).
    pub fn kill(&self) -> bool {
        kill_tree(self)
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        #[cfg(windows)]
        close_job(self.job);
    }
}

/// Puts process `pid` in a new job (Windows), which then holds whatever it starts. With
/// `ends_with_job` the job ends everything in it once its last handle is closed. 0 when it could not.
#[cfg(windows)]
fn job_of(pid: u32, ends_with_job: bool) -> usize {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
    // SAFETY: every handle is checked before use and closed here unless it is returned; the limits are
    // a plain structure that lives until the call returns.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return 0;
        }
        if ends_with_job {
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
        }
        let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            CloseHandle(job);
            return 0;
        }
        let assigned = AssignProcessToJobObject(job, process);
        CloseHandle(process);
        if assigned == 0 {
            CloseHandle(job);
            return 0;
        }
        job as usize
    }
}

#[cfg(windows)]
fn close_job(job: usize) {
    if job != 0 {
        // SAFETY: the handle is one `job_of` returned and nobody else closes.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(job as windows_sys::Win32::Foundation::HANDLE);
        }
    }
}

#[cfg(windows)]
fn kill_tree(tree: &ProcessTree) -> bool {
    if tree.job == 0 {
        return false;
    }
    // SAFETY: the handle is one `job_of` returned and is open while `tree` lives.
    unsafe {
        windows_sys::Win32::System::JobObjects::TerminateJobObject(
            tree.job as windows_sys::Win32::Foundation::HANDLE,
            1,
        ) != 0
    }
}

#[cfg(unix)]
fn kill_tree(tree: &ProcessTree) -> bool {
    let Ok(pid) = libc::pid_t::try_from(tree.pid) else {
        return false;
    };
    if pid <= 1 {
        return false;
    }
    if tree.grouped {
        // SAFETY: the group is the child's own (it was started as its leader), never this process's.
        return unsafe { libc::killpg(pid, libc::SIGKILL) == 0 };
    }
    // The program in front shares its group with this process: what it started is found by asking who
    // is whose child, before it is gone and they are nobody's.
    let below = descendants(tree.pid);
    // SAFETY: kill takes plain values; each pid is a positive process id, never a group.
    unsafe {
        let ended = libc::kill(pid, libc::SIGKILL) == 0;
        for child in below {
            if let Ok(child) = libc::pid_t::try_from(child)
                && child > 1
            {
                libc::kill(child, libc::SIGKILL);
            }
        }
        ended
    }
}

/// Every process below `pid`, by what `ps` says of who started whom.
#[cfg(unix)]
fn descendants(pid: u32) -> Vec<u32> {
    let listed = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "ppid="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(listed) = listed else {
        return Vec::new();
    };
    let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            Some((words.next()?.parse().ok()?, words.next()?.parse().ok()?))
        })
        .collect();
    let mut below = Vec::new();
    let mut parents = vec![pid];
    while let Some(parent) = parents.pop() {
        for (child, _) in pairs.iter().filter(|(child, of)| *of == parent && *child != pid) {
            if !below.contains(child) {
                below.push(*child);
                parents.push(*child);
            }
        }
    }
    below
}

/// How the terminal was set before a full-screen program was started on it: put back once the program
/// is gone, however it went, so that what it changed (raw input, no echo, the input modes of the
/// console) does not stay with the shell.
#[derive(Clone, Copy, Debug)]
pub struct TerminalState {
    /// Windows: the modes of the console's input and of its screen.
    #[cfg(windows)]
    modes: (Option<u32>, Option<u32>),
    /// Unix: the terminal's settings.
    #[cfg(unix)]
    settings: libc::termios,
}

/// The terminal's settings now; None when this process has no terminal.
pub fn save_terminal() -> Option<TerminalState> {
    saved()
}

impl TerminalState {
    /// Puts the terminal back as it was when this was saved. Best effort.
    pub fn restore(&self) {
        put_back(self);
    }
}

/// Throws away what was typed and not yet read: keys meant for a program that is gone must not reach
/// the next one, nor answer a question that was not asked yet.
pub fn flush_input() {
    flush();
}

/// The console's input (`CONIN$`) or screen (`CONOUT$`), opened anew: the standard handles may be
/// redirected.
#[cfg(windows)]
fn console(name: &str) -> Option<std::fs::File> {
    std::fs::OpenOptions::new().read(true).write(true).open(name).ok()
}

#[cfg(windows)]
fn console_mode(name: &str) -> Option<u32> {
    use std::os::windows::io::AsRawHandle;
    let console = console(name)?;
    let mut mode = 0u32;
    // SAFETY: the handle is this function's own open console; the mode is written to a local.
    let read = unsafe {
        windows_sys::Win32::System::Console::GetConsoleMode(console.as_raw_handle().cast(), &mut mode)
    };
    (read != 0).then_some(mode)
}

#[cfg(windows)]
fn saved() -> Option<TerminalState> {
    let modes = (console_mode("CONIN$"), console_mode("CONOUT$"));
    (modes.0.is_some() || modes.1.is_some()).then_some(TerminalState { modes })
}

#[cfg(windows)]
fn put_back(state: &TerminalState) {
    use std::os::windows::io::AsRawHandle;
    for (name, mode) in [("CONIN$", state.modes.0), ("CONOUT$", state.modes.1)] {
        if let (Some(mode), Some(console)) = (mode, console(name)) {
            // SAFETY: the handle is this function's own open console; the mode is a plain value.
            unsafe {
                windows_sys::Win32::System::Console::SetConsoleMode(console.as_raw_handle().cast(), mode);
            }
        }
    }
}

#[cfg(windows)]
fn flush() {
    use std::os::windows::io::AsRawHandle;
    if let Some(console) = console("CONIN$") {
        // SAFETY: the handle is this function's own open console input.
        unsafe {
            windows_sys::Win32::System::Console::FlushConsoleInputBuffer(console.as_raw_handle().cast());
        }
    }
}

#[cfg(unix)]
fn saved() -> Option<TerminalState> {
    // SAFETY: tcgetattr fills a structure this function owns; standard input is only asked about.
    unsafe {
        let mut settings: libc::termios = std::mem::zeroed();
        (libc::tcgetattr(libc::STDIN_FILENO, &mut settings) == 0).then_some(TerminalState { settings })
    }
}

#[cfg(unix)]
fn put_back(state: &TerminalState) {
    // SAFETY: tcsetattr reads a structure that lives until the call returns.
    unsafe {
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &state.settings);
    }
}

#[cfg(unix)]
fn flush() {
    // SAFETY: tcflush takes plain values; on something that is no terminal it does nothing.
    unsafe {
        libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_tree_is_ended_with_what_it_started() {
        // A shell that starts a second process and waits for it.
        let mut command = if cfg!(windows) {
            let mut command = std::process::Command::new("cmd");
            command.args(["/C", "ping -n 30 127.0.0.1 >NUL"]);
            command
        } else {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", "sleep 30 & wait"]);
            command
        };
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        own_group(&mut command);
        let mut child = command.spawn().unwrap();
        let tree = ProcessTree::background(child.id());
        // What it starts is there a moment later.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(tree.kill(), "the system took the request for the whole tree");
        let status = child.wait().unwrap();
        assert!(!status.success());
        assert!(!is_alive(i64::from(child.id())));
        // Ending it again, or a tree that is long gone, is nothing.
        tree.kill();
        ProcessTree::foreground(child.id()).kill();
    }

    #[test]
    fn the_terminal_can_be_saved_and_put_back_and_its_input_thrown_away() {
        // With no terminal (a test run under a pipe) there is nothing to save, and nothing fails.
        if let Some(state) = save_terminal() {
            state.restore();
        }
        flush_input();
        if !cfg!(windows) {
            assert_eq!(type_console_keys(&[3], 0), "no console");
        }
    }

    #[test]
    fn this_process_is_alive_and_nonsense_is_not() {
        assert!(is_alive(i64::from(std::process::id())));
        assert!(!is_alive(0));
        assert!(!is_alive(-4));
        assert!(!is_alive(i64::MAX));
    }

    #[test]
    fn nobody_asked_this_process_to_end_until_somebody_does() {
        watch_end_requests();
        assert_eq!(end_requested(), None);
        note_end_request(143);
        assert_eq!(end_requested(), Some(143));
        end_handled();
        note_end_request(0);
        assert_eq!(end_requested(), None);
    }

    #[test]
    fn interrupts_can_be_ignored_and_restored() {
        ignore_interrupts(true);
        ignore_interrupts(false);
        ignore_interrupts(false);
    }

    #[test]
    fn a_child_that_exited_is_not_alive() {
        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "exit 0"])
                .spawn()
                .unwrap()
        } else {
            std::process::Command::new("true").spawn().unwrap()
        };
        let pid = i64::from(child.id());
        child.wait().unwrap();
        assert!(!is_alive(pid));
    }
}
