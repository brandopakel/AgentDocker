//! A real session owner must reap its orphans even if an outer init does not.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug)]
struct Process {
    pid: u32,
    parent: u32,
    birth: u64,
    state: char,
}

fn process(pid: u32) -> Option<Process> {
    let text = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("cannot inspect owned process {pid}: {error}"),
    };
    let fields: Vec<_> = text
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    Some(Process {
        pid,
        parent: fields[1].parse().unwrap(),
        birth: fields[19].parse().unwrap(),
        state: fields[0].chars().next().unwrap(),
    })
}

struct Adoption(i32);

impl Adoption {
    fn enable() -> Self {
        let mut previous = 0_i32;
        // SAFETY: GET writes one integer; SET takes an integer and no pointers.
        unsafe {
            assert_eq!(libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous), 0);
            assert_eq!(libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0), 0);
        }
        Self(previous)
    }
}

impl Drop for Adoption {
    fn drop(&mut self) {
        // SAFETY: restore only this test process's prior setting after cleanup.
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.0, 0, 0, 0) };
    }
}

fn rpc(socket: &Path, request: Value) -> std::io::Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

fn wait<T>(label: &str, mut observe: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(value) = observe() {
            return value;
        }
        assert!(Instant::now() < end, "timed out: {label}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Fixture {
    daemon: Child,
    socket: PathBuf,
    root: tempfile::TempDir,
    agent: Option<String>,
    descendant: Option<Process>,
    owner: Option<Process>,
    owner_suspended: bool,
}

impl Fixture {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("state");
        let socket = root.path().join("daemon.sock");
        let log = std::fs::File::create(root.path().join("daemon.log")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentd"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AGENTDOCKER_") {
                command.env_remove(key);
            }
        }
        let daemon = command
            .arg("--home")
            .arg(&home)
            .arg("--socket")
            .arg(&socket)
            .env("AGENTDOCKER_HOME", &home)
            .env("AGENTDOCKER_SOCKET", &socket)
            .env("AGENTDOCKER_NO_AUTOSTART", "1")
            .env("AGENTDOCKER_NO_NOTIFICATIONS", "1")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let mut fixture = Self {
            daemon,
            socket,
            root,
            agent: None,
            descendant: None,
            owner: None,
            owner_suspended: false,
        };
        wait("private daemon startup", || {
            assert!(fixture.daemon.try_wait().unwrap().is_none());
            rpc(&fixture.socket, json!({"op":"ping"}))
                .ok()
                .filter(|reply| reply["type"] == "pong")
        });
        fixture
    }

    fn resume_owner(&mut self) {
        if self.owner_suspended {
            let owner = self.owner.unwrap();
            let current = process(owner.pid).expect("owned session owner disappeared");
            assert_eq!(current.birth, owner.birth);
            // SAFETY: this is the same private owner generation we stopped.
            assert_eq!(unsafe { libc::kill(owner.pid as i32, libc::SIGCONT) }, 0);
            self.owner_suspended = false;
        }
    }

    fn call(&self, request: Value) -> Value {
        let reply = rpc(&self.socket, request).unwrap();
        assert_ne!(reply["type"], "error", "{reply}");
        reply
    }
}

/// Cleanup also works against the old daemon: only our exact adopted child is
/// reaped. Never consume arbitrary waitpid(-1) results or another group's child.
fn retire_adopted(expected: Process) -> bool {
    if let Some(current) = process(expected.pid)
        && current.birth == expected.birth
        && current.parent == std::process::id()
    {
        // SAFETY: the unreaped, birth-matched direct child reserves this PID.
        unsafe { libc::kill(expected.pid as i32, libc::SIGKILL) };
        let end = Instant::now() + Duration::from_secs(3);
        loop {
            // SAFETY: only the exact adopted child; nonblocking and no pointers.
            let result =
                unsafe { libc::waitpid(expected.pid as i32, std::ptr::null_mut(), libc::WNOHANG) };
            if result == expected.pid as i32 || process(expected.pid).is_none() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    process(expected.pid).is_none()
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.resume_owner();
        if let Some(agent) = &self.agent {
            // An already-finished reply is harmless during fixture retirement.
            let _ = rpc(
                &self.socket,
                json!({"op":"stop","agent":agent,"force":true}),
            );
        }
        if let Some(child) = self.descendant {
            let _ = retire_adopted(child);
        }
        let _ = rpc(&self.socket, json!({"op":"shutdown"}));
        let end = Instant::now() + Duration::from_secs(10);
        while self.daemon.try_wait().ok().flatten().is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        let forced = self.daemon.try_wait().ok().flatten().is_none();
        if forced {
            let _ = self.daemon.kill();
        }
        let _ = self.daemon.wait();
        let owner_gone = self.owner.is_none_or(retire_adopted);
        let child_gone = self.descendant.is_none_or(retire_adopted);
        if !std::thread::panicking() {
            assert!(
                !forced && owner_gone && child_gone,
                "fixture cleanup was incomplete or forced"
            );
        }
    }
}

#[test]
fn dedicated_owner_reaps_orphans_before_reporting_exit_and_releasing_leases() {
    // No other test lives in this binary. The setting is private to this
    // process, does not propagate through fork, and is restored afterward.
    let _adoption = Adoption::enable();
    let mut fixture = Fixture::start();
    let launched = fixture.call(json!({"op":"run","spec":{
        "name":"orphan-fixture", "runtime":"fixture", "workdir":fixture.root.path(),
        "command":["sh","-c","trap '' TERM; sleep 30 & echo $! > descendant.pid; while [ ! -f exit-leader ]; do sleep 0.01; done"]
    }}));
    let agent = launched["agent"]["id"].as_str().unwrap().to_owned();
    fixture.agent = Some(agent.clone());
    let owner = launched["agent"]["owner"]["pid"].as_u64().unwrap() as u32;
    fixture.owner = Some(process(owner).unwrap());
    let pid = wait("descendant ready", || {
        std::fs::read_to_string(fixture.root.path().join("descendant.pid"))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
    });
    fixture.descendant = Some(process(pid).unwrap());
    assert_eq!(
        fixture.call(json!({"op":"claim","agent":agent,"resource":"task:orphan","ttl_secs":60}))["type"],
        "lease"
    );
    // Freeze only the pinned private owner, so adoption is observed before
    // its shutdown timer can run, even on a loaded CI runner.
    assert_eq!(process(owner).unwrap().birth, fixture.owner.unwrap().birth);
    // SAFETY: the fixture owns this birth-matched session-owner process.
    assert_eq!(unsafe { libc::kill(owner as i32, libc::SIGSTOP) }, 0);
    fixture.owner_suspended = true;
    wait("private owner stopped", || {
        process(owner).filter(|value| value.state == 'T')
    });
    std::fs::write(fixture.root.path().join("exit-leader"), "").unwrap();
    let adopted = wait("owner adopts orphan before ending it", || {
        process(pid)
            .filter(|child| child.parent != launched["agent"]["pid"].as_u64().unwrap() as u32)
    });
    assert_eq!(
        adopted.parent, owner,
        "owner delegated orphan retirement to an external init"
    );
    assert_eq!(
        fixture.call(json!({"op":"leases","agent":agent}))["leases"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "live descendants must retain protection"
    );
    fixture.resume_owner();
    wait("owner releases protection after reaping", || {
        fixture.call(json!({"op":"leases","agent":agent}))["leases"]
            .as_array()
            .unwrap()
            .is_empty()
            .then_some(())
    });
    assert!(
        process(pid).is_none(),
        "descendant was only killed, not reaped"
    );
    let finished = fixture.call(json!({"op":"inspect","agent":agent}));
    assert_eq!(
        finished["agent"]["status"],
        json!({"state":"exited","code":0}),
        "the leader's real exit status must survive descendant reaping"
    );
    wait("owner retires after durable acknowledgement", || {
        process(owner).is_none().then_some(())
    });
}
