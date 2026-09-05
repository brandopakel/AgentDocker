//! Real Unix-socket workload. Emits Bencher Metric Format on stdout.
use agentdocker_core::{AgentSpec, LeaseMode, Request, Response};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
struct DaemonChild(Child);
impl Drop for DaemonChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn request(socket: &Path, request: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let binary = args
        .get(1)
        .context("usage: socket_load /path/to/agentd [clients=10] [iterations=100]")?;
    let clients: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(10);
    let iterations: usize = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(100);
    anyhow::ensure!(
        (1..=1000).contains(&clients) && (1..=100_000).contains(&iterations),
        "workload out of bounds"
    );
    anyhow::ensure!(
        clients
            .checked_mul(iterations)
            .is_some_and(|samples| samples <= 1_000_000),
        "workload retains at most 1,000,000 latency samples"
    );
    let tmp = tempfile::Builder::new()
        .prefix("ad-load-")
        .tempdir_in("/tmp")?;
    let socket = tmp.path().join("sock");
    let checkout = tmp.path().join("checkout");
    std::fs::create_dir(&checkout)?;
    std::fs::write(checkout.join("input.rs"), "original\n")?;
    let log = std::fs::File::create(tmp.path().join("daemon.log"))?;
    let mut daemon = DaemonChild(
        Command::new(binary)
            .arg("--home")
            .arg(tmp.path().join("state"))
            .arg("--socket")
            .arg(&socket)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if matches!(request(&socket, &Request::Ping), Ok(Response::Pong { .. })) {
            break;
        }
        if let Some(status) = daemon.0.try_wait()? {
            bail!(
                "daemon exited: {status}; {}",
                std::fs::read_to_string(tmp.path().join("daemon.log"))?
            );
        }
        anyhow::ensure!(Instant::now() < deadline, "daemon startup timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(clients + 1));
    let mut workers = Vec::new();
    for n in 0..clients {
        let socket = socket.clone();
        let checkout = checkout.clone();
        let barrier = barrier.clone();
        // Register before launching workers so a failed registration cannot strand a barrier.
        let agent = match request(
            &socket,
            &Request::Register {
                spec: AgentSpec {
                    name: format!("load-{n}"),
                    workdir: Some(checkout.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
            },
        )? {
            Response::Agent { agent } => agent.id.to_string(),
            other => bail!("registration: {other:?}"),
        };
        workers.push(std::thread::spawn(move || -> Result<(Vec<f64>, usize)> {
            barrier.wait();
            let mut samples = Vec::new();
            let mut conflicts = 0;
            for _ in 0..iterations {
                let started = Instant::now();
                let reply = request(
                    &socket,
                    &Request::Claim {
                        agent: agent.clone(),
                        resource: format!("path:{}", checkout.join("input.rs").display()),
                        mode: LeaseMode::Exclusive,
                        ttl_secs: 60,
                        note: None,
                        wait_secs: 0,
                    },
                )?;
                match reply {
                    Response::Lease { lease } => {
                        let reply = request(
                            &socket,
                            &Request::Release {
                                summary: None,
                                summary_source: agentdocker_core::SummarySource::Explicit,
                                agent: agent.clone(),
                                lease: lease.id,
                            },
                        )?;
                        anyhow::ensure!(
                            matches!(reply, Response::Lease { .. }),
                            "release: {reply:?}"
                        );
                    }
                    Response::Error {
                        code: agentdocker_core::ErrorCode::Conflict,
                        ..
                    } => conflicts += 1,
                    other => bail!("claim: {other:?}"),
                }
                samples.push(started.elapsed().as_secs_f64() * 1e9);
            }
            Ok((samples, conflicts))
        }));
    }
    let start = Instant::now();
    barrier.wait();
    let mut samples = Vec::new();
    let mut conflicts = 0;
    let mut error = None;
    for worker in workers {
        match worker.join() {
            Ok(Ok((mut values, count))) => {
                samples.append(&mut values);
                conflicts += count;
            }
            Ok(Err(e)) => error = Some(e),
            Err(_) => error = Some(anyhow::anyhow!("load worker panicked")),
        }
    }
    if let Some(error) = error {
        return Err(error);
    }
    let elapsed = start.elapsed().as_secs_f64();
    samples.sort_by(f64::total_cmp);
    let percentile = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
    let name = format!("socket_claim_release/{clients}_clients/{iterations}_iterations");
    println!(
        "{}",
        json!({
            format!("{name}/p50"): {"latency": {"value": percentile(0.50)}},
            format!("{name}/p95"): {"latency": {"value": percentile(0.95)}},
            format!("{name}/p99"): {"latency": {"value": percentile(0.99)}},
            format!("{name}/throughput"): {"throughput": {"value": samples.len() as f64 / elapsed}},
        })
    );
    eprintln!(
        "{clients} clients, {} operations, {conflicts} expected conflicts, {elapsed:.3}s total; latency unit ns; includes connection setup and successful release",
        samples.len()
    );
    Ok(())
}
