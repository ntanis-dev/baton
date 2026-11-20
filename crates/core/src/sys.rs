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

/// From now on a request to end this process (Ctrl+Break on Windows; SIGTERM and SIGHUP elsewhere) is
/// noted for [`end_requested`] instead of ending it at once, so that a full-screen program can give
/// the terminal back first.
pub fn watch_end_requests() {
    watch_ends();
}

/// The exit code of a request to end this process that arrived since [`watch_end_requests`]: 149 for
/// Ctrl+Break, 143 for SIGTERM, 129 for SIGHUP. None when nobody asked.
pub fn end_requested() -> Option<i32> {
    let code = END_REQUEST.load(std::sync::atomic::Ordering::SeqCst);
    (code != 0).then_some(code)
}

/// A console control handler that notes Ctrl+Break and leaves everything else to the next handler.
#[cfg(windows)]
unsafe extern "system" fn note_break(kind: u32) -> windows_sys::core::BOOL {
    use windows_sys::Win32::System::Console::CTRL_BREAK_EVENT;
    if kind != CTRL_BREAK_EVENT {
        return 0;
    }
    END_REQUEST.store(149, std::sync::atomic::Ordering::SeqCst);
    1
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_and_nonsense_is_not() {
        assert!(is_alive(i64::from(std::process::id())));
        assert!(!is_alive(0));
        assert!(!is_alive(-4));
        assert!(!is_alive(i64::MAX));
    }

    #[test]
    fn nobody_asked_this_process_to_end() {
        watch_end_requests();
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
