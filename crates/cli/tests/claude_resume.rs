//! A Claude Code session the person resumes from their own terminal, whose
//! MCP server starts before its SessionStart hook registers, comes back under
//! its ended record: the server waits for the hook, which resumes the record
//! from outside every agent's session, and joins it. Every process belongs to
//! the fixture — a staged `claude --resume` (a script under a path naming
//! Claude Code), the real `agentdocker mcp` and `agentdocker hook`, and a
//! private daemon with the agent check on; no installed daemon, provider
//! account or user config is used.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct RunningDaemon {
    child: Child,
    socket: PathBuf,
}

impl RunningDaemon {
    fn start(home: &Path, socket: &Path) -> Self {
        let log =
            agentdocker_host::dirs::private_file(&home.with_extension("daemon.log"), true, true)
                .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
            .args(["daemon", "run"])
            .arg("--home")
            .arg(home)
            .arg("--socket")
            .arg(socket)
            // The harness acts as the person from wherever it runs, an
            // agent's session included. The agent check stays on: it is
            // what refuses a hook that registers second.
            .env("AGENTDOCKER_PERSON_CHECK", "off")
            .env_remove("AGENTDOCKER_AGENT_CHECK")
            .env("AGENTDOCKER_HOME", home)
            .env("AGENTDOCKER_SOCKET", socket)
            .env("AGENTDOCKER_NO_AUTOSTART", "1")
            .env_remove("AGENTDOCKER_TOKEN_FILE")
            .env_remove("AGENTDOCKER_AGENT_ID")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let mut running = Self {
            child,
            socket: socket.to_path_buf(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                running.child.try_wait().unwrap().is_none(),
                "daemon exited at startup"
            );
            if rpc(socket, json!({"op": "ping"})).is_ok_and(|response| response["type"] == "pong") {
                return running;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        let _ = rpc(&self.socket, json!({"op": "shutdown"}));
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The staged session, ended with whatever it started.
struct Staged(Child);

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn rpc(socket: &Path, request: Value) -> std::io::Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

/// `request`'s answer, sent from outside every session — from a process whose
/// parent exits before it speaks, as a terminal the person opened is — since
/// the suite may itself run inside an agent's session.
fn from_outside(socket: &Path, request: &Value) -> Value {
    let script = "import os,socket,sys,time\nparent=os.getppid()\nprint('ready',flush=True)\nwhile os.getppid()==parent:\n    time.sleep(0.01)\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\ns.sendall(sys.argv[2].encode()+b'\\n')\nprint(s.makefile().readline(),end='',flush=True)\n";
    let mut parent = Command::new("sh")
        .arg("-c")
        .arg("python3 -c \"$0\" \"$1\" \"$2\" & read line; exit 0")
        .arg(script)
        .arg(socket)
        .arg(request.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(parent.stdout.take().unwrap());
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "ready");
    writeln!(parent.stdin.take().unwrap(), "exit").unwrap();
    parent.wait().unwrap();
    line.clear();
    out.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn a_resumed_session_whose_mcp_server_starts_first_resumes_its_record() {
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("node is not installed here; skipping the staged resumption");
        return;
    }
    // Short enough for a Unix socket path.
    let tmp = tempfile::Builder::new()
        .prefix("ad-resume-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = tmp.path().join("home");
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    let socket = tmp.path().join("d.sock");
    let daemon = RunningDaemon::start(&home, &socket);

    // The session that ended: its record holds a message, its process has
    // exited.
    let mut earlier = Command::new("sleep").arg("30").spawn().unwrap();
    let registered = rpc(
        &socket,
        json!({"op": "register", "pid": earlier.id(), "spec": {
            "name": "claude-c", "runtime": "claude-code", "workdir": work,
            "labels": {"session_id": "session-c"}}}),
    )
    .unwrap();
    assert_eq!(registered["type"], "agent", "{registered}");
    let c = registered["agent"]["id"].as_str().unwrap().to_owned();
    let sent = rpc(
        &socket,
        json!({"op": "send", "from": "user", "to": c, "kind": "chat",
               "payload": "for C"}),
    )
    .unwrap();
    assert_eq!(sent["type"], "sent", "{sent}");
    let _ = earlier.kill();
    let _ = earlier.wait();
    let gone = from_outside(&socket, &json!({"op": "deregister", "agent": c}));
    assert_eq!(gone["type"], "agent", "{gone}");

    // `claude --resume session-c`, staged: it starts its MCP server at
    // once and its SessionStart hook a moment later, as Claude Code does
    // when the server wins the race.
    let cli = work.join("node_modules/@anthropic-ai/claude-code/cli.js");
    std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
    std::fs::write(
        &cli,
        "const {spawn}=require('child_process');\n\
         const [exe,work,said]=process.argv.slice(4);\n\
         spawn(exe,['mcp','--runtime','claude-code'],{cwd:work,stdio:['pipe','ignore','inherit']});\n\
         setTimeout(()=>{\n\
           const hook=spawn(exe,['hook','claude-code'],{cwd:work,stdio:['pipe',require('fs').openSync(said,'w'),'ignore']});\n\
           hook.stdin.end(JSON.stringify({session_id:'session-c',hook_event_name:'SessionStart',source:'resume',cwd:work}));\n\
         },1500);\n\
         setInterval(()=>{},1000);\n",
    )
    .unwrap();
    let stderr = tmp.path().join("mcp.err");
    let said = tmp.path().join("hook.out");
    let staged = Staged(
        Command::new("node")
            .arg(&cli)
            .args(["--resume", "session-c"])
            .arg(env!("CARGO_BIN_EXE_agentdocker"))
            .arg(&work)
            .arg(&said)
            .current_dir(&work)
            .env("AGENTDOCKER_HOME", &home)
            .env("AGENTDOCKER_SOCKET", &socket)
            .env("AGENTDOCKER_NO_AUTOSTART", "1")
            .env_remove("AGENTDOCKER_AGENT_ID")
            .env_remove("AGENTDOCKER_CLAUDE_CHANNEL_INPUT")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap(),
    );
    let session = staged.0.id();

    // C is live again under its own id, on the staged session's process,
    // and its MCP server speaks as C.
    let deadline = Instant::now() + Duration::from_secs(30);
    let serving = loop {
        let log = std::fs::read_to_string(&stderr).unwrap_or_default();
        if let Some(line) = log.lines().find(|line| line.contains("serving as")) {
            break line.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "the MCP server never registered:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let current = rpc(&socket, json!({"op": "inspect", "agent": c})).unwrap();
    assert_eq!(current["type"], "agent", "{current}");
    assert_eq!(current["agent"]["id"], c.as_str(), "{current}");
    assert_eq!(current["agent"]["pid"], session, "C resumed: {current}");
    assert!(
        serving.contains(&format!("serving as claude-c ({c})")),
        "the server speaks as C: {serving}"
    );
    let listed = rpc(&socket, json!({"op": "list"})).unwrap();
    let on_session: Vec<&Value> = listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|agent| agent["pid"] == session)
        .collect();
    assert_eq!(on_session.len(), 1, "one record for the session: {listed}");
    // What was queued for C reaches the session that resumed it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let hook = std::fs::read_to_string(&said).unwrap_or_default();
        if hook.contains("for C") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "C's message never reached its session: {hook}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(staged);
    drop(daemon);
}
