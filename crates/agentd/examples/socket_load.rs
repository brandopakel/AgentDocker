//! Real Unix-socket workload. Emits Bencher Metric Format on stdout.
use agentdocker_core::{AgentSpec, LeaseMode, Request, Response};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
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
    let started = Instant::now();
    let operation = match request {
        Request::Ping => "ping",
        Request::Register { .. } => "register",
        Request::Claim { .. } => "claim",
        Request::Release { .. } => "release",
        _ => "request",
    };
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("{operation}: connect to fixture daemon"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    serde_json::to_writer(&mut stream, request)
        .with_context(|| format!("{operation}: write request"))?;
    stream
        .write_all(b"\n")
        .with_context(|| format!("{operation}: finish request"))?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .with_context(|| format!("{operation}: read response after {:?}", started.elapsed()))?;
    serde_json::from_str(&line).with_context(|| format!("{operation}: decode response"))
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let binary = args.get(1).context(
        "usage: socket_load /path/to/agentd [clients=10] [iterations=100] [shared|disjoint]",
    )?;
    let clients: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(10);
    let iterations: usize = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(100);
    let workload = args.get(4).map(String::as_str).unwrap_or("shared");
    anyhow::ensure!(
        matches!(workload, "shared" | "disjoint") && args.len() <= 5,
        "expected shared or disjoint workload"
    );
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
    let diagnostics = std::env::var("AGENTDOCKER_BENCH_DIAGNOSTICS").as_deref() == Ok("1");
    let mut daemon = DaemonChild(
        Command::new(binary)
            .env(
                "RUST_LOG",
                if diagnostics {
                    "warn,agentd_state_timing=debug"
                } else {
                    "warn"
                },
            )
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
    // Register the entire fixture before starting any waiting worker.
    let mut registered = Vec::with_capacity(clients);
    for n in 0..clients {
        let socket = socket.clone();
        let checkout = checkout.clone();
        let agent = match request(
            &socket,
            &Request::Register {
                spec: AgentSpec {
                    name: format!("load-{n}"),
                    workdir: Some(checkout.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            },
        )? {
            Response::Agent { agent } => agent.id.to_string(),
            other => bail!("registration: {other:?}"),
        };
        let input = if workload == "shared" {
            checkout.join("input.rs")
        } else {
            checkout.join(format!("input-{n}.rs"))
        };
        if workload == "disjoint" {
            std::fs::write(&input, "original\n")?;
        }
        registered.push((agent, format!("path:{}", input.display())));
    }
    let (mut samples, elapsed) = std::thread::scope(|scope| -> Result<_> {
        let mut workers = Vec::new();
        // Dropping these senders cancels workers if creating a later thread fails.
        let mut starts = Vec::new();
        for (agent, resource) in registered {
            let socket = socket.clone();
            let (start, ready) = std::sync::mpsc::channel();
            starts.push(start);
            workers.push(
                std::thread::Builder::new()
                    .spawn_scoped(scope, move || -> Result<Samples> {
                        ready.recv().context("workload start cancelled")?;
                        let mut samples = Samples::default();
                        for _ in 0..iterations {
                            let started = Instant::now();
                            let reply = request(
                                &socket,
                                &Request::Claim {
                                    agent: agent.clone(),
                                    resource: resource.clone(),
                                    mode: LeaseMode::Exclusive,
                                    amount: None,
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
                                            summary_source:
                                                agentdocker_core::SummarySource::Explicit,
                                            agent: agent.clone(),
                                            lease: lease.id,
                                        },
                                    )?;
                                    anyhow::ensure!(
                                        matches!(reply, Response::Lease { .. }),
                                        "release: {reply:?}"
                                    );
                                    samples.success.push(started.elapsed().as_secs_f64() * 1e9);
                                }
                                Response::Error {
                                    code: agentdocker_core::ErrorCode::Conflict,
                                    ..
                                } => {
                                    anyhow::ensure!(
                                        workload == "shared",
                                        "unexpected conflict between disjoint fixture paths"
                                    );
                                    samples.conflict.push(started.elapsed().as_secs_f64() * 1e9);
                                }
                                other => bail!("claim: {other:?}"),
                            }
                        }
                        Ok(samples)
                    })
                    .context("create load worker")?,
            );
        }
        let start = Instant::now();
        for ready in starts {
            ready.send(()).context("start load worker")?;
        }
        let mut samples = Samples::default();
        let mut error = None;
        for worker in workers {
            match worker.join() {
                Ok(Ok(mut values)) => {
                    samples.success.append(&mut values.success);
                    samples.conflict.append(&mut values.conflict);
                }
                Ok(Err(e)) => {
                    error.get_or_insert(e);
                }
                Err(_) => {
                    error.get_or_insert(anyhow::anyhow!("load worker panicked"));
                }
            }
        }
        if let Some(error) = error {
            return Err(error);
        }
        Ok((samples, start.elapsed().as_secs_f64()))
    })
    .with_context(|| {
        format!(
            "{clients}-client workload failed; fixture daemon log: {}",
            log_tail(&tmp.path().join("daemon.log"))
        )
    })?;
    let name = format!("socket_v2/{workload}/{clients}_clients/{iterations}_iterations");
    let attempts = samples.success.len() + samples.conflict.len();
    anyhow::ensure!(attempts == clients * iterations, "incomplete workload");
    let mut metrics = Map::new();
    series(
        &mut metrics,
        &format!("{name}/claim_release"),
        &mut samples.success,
        elapsed,
    );
    series(
        &mut metrics,
        &format!("{name}/claim_conflict"),
        &mut samples.conflict,
        elapsed,
    );
    metrics.insert(
        format!("{name}/attempts"),
        json!({
            "sample-count": {"value": attempts},
            "throughput": {"value": attempts as f64 / elapsed},
            "elapsed-seconds": {"value": elapsed},
            "conflict-ratio": {"value": samples.conflict.len() as f64 / attempts as f64},
        }),
    );
    println!("{}", Value::Object(metrics));
    eprintln!(
        "{clients} clients, {workload} paths, {attempts} attempts, {} successful claim/releases, {} expected conflicts, {elapsed:.3}s total; latency unit ns; includes connection setup; outcomes measured separately",
        samples.success.len(),
        samples.conflict.len()
    );
    if diagnostics {
        eprintln!(
            "bounded state timing tail (diagnostic run): {}",
            log_tail(&tmp.path().join("daemon.log"))
        );
    }
    Ok(())
}

#[derive(Default)]
struct Samples {
    success: Vec<f64>,
    conflict: Vec<f64>,
}

fn series(metrics: &mut Map<String, Value>, name: &str, samples: &mut [f64], elapsed: f64) {
    metrics.insert(
        name.into(),
        json!({
            "sample-count": {"value": samples.len()},
            "throughput": {"value": samples.len() as f64 / elapsed},
        }),
    );
    // No observations do not mean zero latency. Omit those percentiles.
    if samples.is_empty() {
        return;
    }
    samples.sort_by(f64::total_cmp);
    for (label, percentile) in [("p50", 0.50), ("p95", 0.95), ("p99", 0.99)] {
        let index = ((samples.len() - 1) as f64 * percentile).ceil() as usize;
        metrics.insert(
            format!("{name}/{label}"),
            json!({"latency": {"value": samples[index]}}),
        );
    }
}

fn log_tail(path: &Path) -> String {
    let read = (|| -> std::io::Result<_> {
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(file.metadata()?.len().saturating_sub(8192)))?;
        let mut bytes = Vec::new();
        file.take(8192).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    })();
    read.unwrap_or_else(|error| format!("unavailable: {error}"))
}
