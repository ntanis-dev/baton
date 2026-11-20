//! The poll lease: one baton instance polls at a time, and another takes over when it stops, dies or
//! stops renewing. First with a recorder and a clock the test moves, then with real processes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use baton_claude::poll_host::{PollHost, PollHostOptions, PollWork};
use baton_core::clock::{Ms, iso, now_ms, parse_ms};
use baton_core::config::{load_config, save_config};
use baton_core::fsutil::{is_alive, read_json_lines};
use baton_core::json::Get;
use baton_core::paths::Paths;
use baton_core::poll::{
    Holder, lease_file, poll_holder, poll_holder_with, read_lease, read_poll_state, request_poll,
};
use baton_testkit::fixtures::{FAKE_USAGE_CLAUDE, bin};
use baton_testkit::{TestHome, accounts_home, test_env_with, until_for};
use serde_json::{Value, json};

/// A poller that only records what it was asked.
#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(String, String)>>,
}

impl Recorder {
    fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().clone()
    }
}

impl PollWork for Recorder {
    fn refresh(&self, account: Option<&str>, reason: &str) {
        self.calls
            .lock()
            .unwrap()
            .push((account.unwrap_or("all").to_owned(), reason.to_owned()));
    }

    fn tick(&self) {
        self.calls
            .lock()
            .unwrap()
            .push(("all".to_owned(), "schedule".to_owned()));
    }

    fn stop(&self) {}
}

/// The processes the test calls dead.
type Dead = Arc<Mutex<Vec<i64>>>;

/// A host whose poller is a recorder; the clock and the liveness are the test's.
fn host(
    paths: &Paths,
    kind: &str,
    pid: i64,
    clock: &Arc<AtomicI64>,
    dead: &Dead,
) -> (PollHost, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let mut options = PollHostOptions::new(paths.clone(), kind, baton_core::env::Env::new());
    options.pid = pid;
    options.work = Some(Arc::clone(&recorder) as Arc<dyn PollWork>);
    let time = Arc::clone(clock);
    options.now = Arc::new(move || time.load(Ordering::SeqCst));
    let gone = Arc::clone(dead);
    options.alive = Arc::new(move |pid: i64| !gone.lock().unwrap().contains(&pid));
    (PollHost::new(options), recorder)
}

fn pair(account: &str, reason: &str) -> (String, String) {
    (account.to_owned(), reason.to_owned())
}

#[test]
fn one_baton_instance_holds_the_poll_lease_and_polls_another_takes_over_when_it_stops_dies_or_stops_renewing()
{
    let home = accounts_home(&["acct1", "acct2"]);
    let paths = &home.paths;
    let start = parse_ms("2026-10-25T10:00:00Z").unwrap();
    let clock = Arc::new(AtomicI64::new(start));
    let advance = |by: Ms| clock.fetch_add(by, Ordering::SeqCst) + by;
    let dead: Dead = Arc::new(Mutex::new(Vec::new()));
    let gone = Arc::clone(&dead);
    let alive = move |pid: i64| !gone.lock().unwrap().contains(&pid);
    let holder = |now: Ms| poll_holder_with(paths, now, &alive);
    let (a, a_calls) = host(paths, "menu", 101, &clock, &dead);
    let (b, b_calls) = host(paths, "start", 102, &clock, &dead);
    let (c, c_calls) = host(paths, "start", 103, &clock, &dead);
    assert!(a.beat());
    assert!(!b.beat());
    assert!(!c.beat());
    for _ in 0..5 {
        advance(3000);
        a.beat();
        b.beat();
        c.beat();
    }
    // Only the holder ran the schedule.
    assert_eq!(a_calls.calls().len(), 6);
    assert!(b_calls.calls().is_empty() && c_calls.calls().is_empty());
    let now = clock.load(Ordering::SeqCst);
    assert_eq!(
        holder(now),
        Some(Holder {
            kind: "menu".to_owned(),
            pid: 101,
            since: Some(iso(start))
        })
    );
    assert!(a.holds() && !b.holds());
    assert!(a.holding() && !b.holding());
    // A command asks for an account's poll: the holder takes it once.
    // (Request files sort by their millisecond: two in one millisecond would come in a random order.)
    let asked = now_ms();
    request_poll(paths, "acct2", "renamed", asked);
    request_poll(paths, "acct2", "signed in", asked + 1);
    b.beat();
    assert!(b_calls.calls().is_empty(), "only the holder takes requests");
    a.beat();
    let calls = a_calls.calls();
    assert_eq!(
        calls[calls.len() - 2..],
        [pair("acct2", "renamed"), pair("all", "schedule")]
    );
    // The holder dies: the next instance takes the lease at once.
    dead.lock().unwrap().push(101);
    assert!(b.beat());
    assert!(!c.beat());
    assert_eq!(holder(clock.load(Ordering::SeqCst)).unwrap().pid, 102);
    // The holder stops renewing (hung): taken after 10 s.
    advance(9000);
    assert!(!c.beat());
    advance(2000);
    assert!(c.beat());
    assert!(!b.beat(), "it lost the lease and knows it");
    assert!(!b.holds());
    // The holder stops: it lets the lease go, and the last one leaves none.
    c.stop();
    let now = clock.load(Ordering::SeqCst);
    assert_eq!(holder(now), None);
    assert!(!c.beat(), "a stopped instance holds nothing");
    assert!(b.beat());
    b.stop();
    a.stop();
    assert!(!lease_file(paths).exists(), "nobody polls");
    assert_eq!(holder(now), None);
}

#[test]
fn a_started_host_beats_by_itself_and_lets_the_lease_go_when_it_is_dropped() {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    let recorder = Arc::new(Recorder::default());
    let mut options = PollHostOptions::new(paths.clone(), "menu", baton_core::env::Env::new());
    options.work = Some(Arc::clone(&recorder) as Arc<dyn PollWork>);
    options.heartbeat_ms = 20;
    let host = PollHost::new(options);
    assert!(host.poller().is_none(), "it was given the polling to drive");
    host.start();
    assert!(host.holds(), "the first heartbeat is at once");
    until_for(Duration::from_secs(10), || {
        (recorder.calls().len() >= 3).then_some(())
    });
    let me = i64::from(std::process::id());
    assert_eq!(
        poll_holder(paths, now_ms()).map(|holder| (holder.kind, holder.pid)),
        Some(("menu".to_owned(), me))
    );
    drop(host);
    assert!(!lease_file(paths).exists());
    let after = recorder.calls().len();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(recorder.calls().len(), after, "no heartbeat after it stopped");
}

/// Real processes: instances on one home, the fake usage claude.
struct Instances {
    list: Vec<Child>,
    log: PathBuf,
    claudes: PathBuf,
}

impl Instances {
    fn start(n: usize, home: &TestHome) -> Instances {
        let root = home.home.parent().unwrap();
        let log = root.join("instances.jsonl");
        let claudes = root.join("claudes.jsonl");
        let fake = bin(FAKE_USAGE_CLAUDE);
        let list = (0..n)
            .map(|i| {
                let env = test_env_with(
                    &home.home,
                    &[
                        ("INSTANCE_LOG", &log.to_string_lossy()),
                        ("INSTANCE_KIND", if i == 0 { "menu" } else { "start" }),
                        ("FAKE_USAGE_CLAUDE", &fake.to_string_lossy()),
                        ("FAKE_USAGE_LOG", &claudes.to_string_lossy()),
                        ("HEARTBEAT_MS", "150"),
                        ("STALE_MS", "1500"),
                    ],
                );
                let mut command = Command::new(env!("CARGO_BIN_EXE_poll-instance"));
                env.apply(&mut command);
                command
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        Instances { list, log, claudes }
    }

    fn events(&self, event: &str) -> Vec<Value> {
        read_json_lines(&self.log)
            .into_iter()
            .filter(|e| e.text("event") == Some(event))
            .collect()
    }

    fn child(&mut self, pid: i64) -> &mut Child {
        self.list
            .iter_mut()
            .find(|child| i64::from(child.id()) == pid)
            .unwrap()
    }

    fn claudes_alive(&self) -> Vec<i64> {
        read_json_lines(&self.claudes)
            .iter()
            .filter_map(|c| c.int("pid"))
            .filter(|pid| is_alive(*pid))
            .collect()
    }
}

impl Drop for Instances {
    fn drop(&mut self) {
        for child in &mut self.list {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn several_instances_exactly_one_polls_another_takes_over_when_it_exits_or_is_killed_nothing_runs_after_the_last_one()
 {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    // A poll every 0.6 s, so that each holder's turn shows.
    let mut config = load_config(paths);
    config.policy.insert("usagePollMinutes".to_owned(), json!(0.01));
    save_config(paths, &config).unwrap();
    let mut run = Instances::start(3, &home);
    let holder = || read_lease(paths).and_then(|lease| lease.pid);
    let secs = Duration::from_secs;
    until_for(secs(20), || (run.events("ready").len() == 3).then_some(()));
    until_for(secs(30), || (run.events("polled").len() >= 3).then_some(()));
    let first = holder().unwrap();
    let mut pollers: Vec<i64> = run.events("polled").iter().filter_map(|e| e.int("pid")).collect();
    pollers.dedup();
    assert_eq!(pollers, [first], "one instance polls");
    assert_eq!(
        read_poll_state(paths).accounts["acct1"].status.as_deref(),
        Some("ok")
    );
    // Killed: another live instance takes over and polls.
    let victim = run.child(first);
    victim.kill().unwrap();
    victim.wait().unwrap();
    let second = until_for(secs(10), || holder().filter(|pid| *pid != first));
    let killed_at = now_ms();
    until_for(secs(20), || {
        run.events("polled")
            .iter()
            .any(|e| e.int("pid") == Some(second) && e.int("at").is_some_and(|at| at > killed_at))
            .then_some(())
    });
    // Exits (its stdin closes): the last one takes over.
    let leaving = run.child(second);
    drop(leaving.stdin.take());
    assert!(leaving.wait().unwrap().success());
    assert!(run.events("stopped").iter().any(|e| e.int("pid") == Some(second)));
    let third = until_for(secs(10), || holder().filter(|pid| *pid != second));
    assert!(third != first && third != second);
    // The last instance exits: no lease, and no process of theirs survives.
    let last = run.child(third);
    drop(last.stdin.take());
    assert!(last.wait().unwrap().success());
    assert!(!lease_file(paths).exists());
    assert_eq!(poll_holder(paths, now_ms()), None);
    for child in &run.list {
        assert!(!is_alive(i64::from(child.id())), "instance {}", child.id());
    }
    until_for(secs(10), || run.claudes_alive().is_empty().then_some(()));
    assert!(
        !read_json_lines(&run.claudes).is_empty(),
        "the reads ran a claude of their own"
    );
}
