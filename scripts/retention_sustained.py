#!/usr/bin/env python3
"""Sustained use of one private daemon with journal retention enabled.

A fixed population of registered agents claims and releases leases with
summaries (journal entries), sends and acknowledges messages, and saves
checkpoints, for a set time, against a daemon whose agentd.toml keeps only a
short journal retention window. Half the agents finish midway so checkpoint
pruning has something it may delete and something it must keep. Throughout,
a reader with a cursor from before the first prune keeps reading in order.

Asserts: retention pruned the journal by itself (journal_pruned with reason
retention, on the event stream), the oldest retained entry is inside the
window plus one tick, every journal read stayed monotonic in head_seq and in
order past its cursor, finished agents' old checkpoints were pruned while
live agents' were kept, and the database and the daemon's memory stopped
growing once retention took hold. Writes result.json under --output.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import traceback


def rpc(sock, request, timeout=30):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(timeout)
        connection.connect(str(sock))
        connection.sendall(json.dumps(request).encode() + b"\n")
        with connection.makefile("rb") as reply:
            return json.loads(reply.readline())


def rss_kib(pid):
    out = subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)], text=True).strip()
    return int(out) if out else None


def db_bytes(home):
    total = 0
    for name in ("state.db", "state.db-wal"):
        path = home / name
        if path.exists():
            total += path.stat().st_size
    return total


def journal_rows(home):
    con = sqlite3.connect(f"file:{home / 'state.db'}?mode=ro", uri=True)
    try:
        count, oldest = con.execute("select count(*), min(at) from journal").fetchone()
        checkpoints = con.execute("select count(*) from documents where kind = 'checkpoint'").fetchone()[0]
    finally:
        con.close()
    return count, oldest, checkpoints


def trial(args):
    args.output.mkdir(parents=True, exist_ok=True)
    binary_dir = args.binary_dir.resolve(strict=True)
    agentd = binary_dir / "agentd"
    result = {"passed": False, "seconds": args.seconds, "agents": args.agents, "retention": args.retention,
              "build_info": json.loads(subprocess.check_output([str(agentd), "--build-info"], text=True)),
              "scenarios": [], "samples": []}
    with tempfile.TemporaryDirectory(prefix="ad-retention-") as temporary:
        root = Path(temporary).resolve()
        home = root / "state"
        home.mkdir()
        (home / "agentd.toml").write_text(f'[journal]\nretention = "{args.retention}"\n')
        sock = root / "agentd.sock"
        work = root / "work"
        work.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=work, check=True)
        subprocess.run(["git", "-c", "user.name=retention", "-c", "user.email=retention@localhost",
                        "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "init"], cwd=work, check=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_")}
        env.update(AGENTDOCKER_HOME=str(home), AGENTDOCKER_SOCKET=str(sock), AGENTDOCKER_NO_AUTOSTART="1", RUST_LOG="info")
        log = open(args.output / "daemon.log", "ab")
        daemon = subprocess.Popen([str(agentd), "--home", str(home), "--socket", str(sock)], env=env,
                                  stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
        stop = threading.Event()
        pruned = []          # (at, reason, removed) from journal_pruned events
        checkpoints_pruned = []
        try:
            deadline = time.monotonic() + 15
            while True:
                try:
                    assert rpc(sock, {"op": "ping"})["type"] == "pong"
                    break
                except (OSError, AssertionError):
                    assert daemon.poll() is None and time.monotonic() < deadline, "daemon did not start"
                    time.sleep(.05)

            # Watch the event stream for pruning announcements.
            def watch():
                with socket.socket(socket.AF_UNIX) as connection:
                    connection.connect(str(sock))
                    connection.sendall(json.dumps({"op": "events", "replay": 0, "ready": True}).encode() + b"\n")
                    with connection.makefile("rb") as stream:
                        for line in stream:
                            if stop.is_set():
                                return
                            frame = json.loads(line)
                            if frame.get("type") != "event":
                                continue
                            kind = frame["event"]["kind"]
                            if kind.get("event") == "journal_pruned":
                                pruned.append((frame["event"]["at"], kind["reason"], kind["removed"]))
                            elif kind.get("event") == "checkpoints_pruned":
                                checkpoints_pruned.extend(kind["checkpoints"])
            threading.Thread(target=watch, daemon=True).start()

            agents = []
            for i in range(args.agents):
                r = rpc(sock, {"op": "register", "spec": {"name": f"worker-{i}", "workdir": str(work)}, "pid": None, "session": None})
                assert r["type"] == "agent", r
                agents.append(r["agent"])
            # The journal is addressed by checkout path; the daemon derives the project.
            project = str(work)
            leaving = agents[: args.agents // 2]
            staying = agents[args.agents // 2:]

            # A reader's cursor from before any pruning: it must keep working.
            early = rpc(sock, {"op": "journal", "project": project, "limit": 1})
            assert early["type"] == "journal", early
            early_cursor = early.get("head_seq") or 0
            heads = [early_cursor]
            read_errors = []
            checkpoint_keys = {a["id"]: [] for a in agents}
            errors = []
            cycles = [0]
            lock = threading.Lock()

            leave = {a["id"]: threading.Event() for a in agents}

            def worker(agent):
                n = 0
                while not stop.is_set() and not leave[agent["id"]].is_set():
                    n += 1
                    try:
                        lease = rpc(sock, {"op": "claim", "agent": agent["id"], "resource": f"path:{work}/file-{agent['spec']['name']}.txt",
                                           "ttl_secs": 60, "wait_secs": 0, "note": f"cycle {n}"})
                        assert lease["type"] == "lease", lease
                        released = rpc(sock, {"op": "release", "agent": agent["id"], "lease": lease["lease"]["id"],
                                              "summary": f"{agent['spec']['name']} finished cycle {n}", "summary_source": "explicit"})
                        assert released["type"] != "error", released
                        sent = rpc(sock, {"op": "send", "from": agent["id"], "to": agent["id"], "kind": "chat", "payload": {"text": f"cycle {n}"}})
                        assert sent["type"] == "sent", sent
                        acked = rpc(sock, {"op": "ack_inbox", "agent": agent["id"], "messages": [sent["message"]]})
                        assert acked["type"] == "ok", acked
                        if n % 10 == 0:
                            key = f"cp-{n}"
                            saved = rpc(sock, {"op": "checkpoint", "agent": agent["id"], "key": key, "task": f"cycle {n}",
                                               "assumptions": [], "next_steps": [], "release_leases": False})
                            assert saved["type"] != "error", saved
                            with lock:
                                checkpoint_keys[agent["id"]].append(key)
                        if n % 25 == 0:
                            page = rpc(sock, {"op": "journal", "project": project, "since_seq": early_cursor, "limit": 50})
                            assert page["type"] == "journal", page
                            seqs = [e["seq"] for e in page["entries"]]
                            with lock:
                                if page.get("head_seq") is not None and page["head_seq"] < heads[-1]:
                                    read_errors.append(f"head went backwards {heads[-1]} -> {page['head_seq']}")
                                heads.append(page.get("head_seq") or heads[-1])
                                if seqs != sorted(seqs) or (seqs and seqs[0] <= early_cursor):
                                    read_errors.append(f"read past cursor {early_cursor} out of order: {seqs[:5]}")
                        with lock:
                            cycles[0] += 1
                    except Exception:  # noqa: BLE001 - recorded, and the trial fails on it
                        with lock:
                            errors.append(traceback.format_exc())
                        return
                    time.sleep(args.pause)

            threads = {a["id"]: threading.Thread(target=worker, args=(a,), daemon=True) for a in agents}
            for t in threads.values():
                t.start()
            started = time.monotonic()
            half_done = False
            next_sample = started
            next_prune = started + 60
            while time.monotonic() - started < args.seconds:
                now = time.monotonic()
                if now >= next_sample:
                    count, oldest, cps = journal_rows(home)
                    ping_at = time.monotonic()
                    rpc(sock, {"op": "ping"})
                    result["samples"].append({"at": round(now - started, 1), "rss_kib": rss_kib(daemon.pid), "db_bytes": db_bytes(home),
                                              "journal_rows": count, "oldest_journal_at": oldest, "checkpoints": cps,
                                              "cycles": cycles[0], "ping_ms": round((time.monotonic() - ping_at) * 1000, 2),
                                              "prunes_seen": len(pruned)})
                    next_sample = now + args.sample
                if not half_done and now - started >= args.seconds / 2:
                    # Half the population finishes: their workers stop first
                    # (deregistering releases an agent's leases), then their
                    # checkpoints become prunable.
                    for agent in leaving:
                        leave[agent["id"]].set()
                    for agent in leaving:
                        threads[agent["id"]].join(timeout=60)
                        assert not threads[agent["id"]].is_alive(), "a leaving worker did not stop"
                    for agent in leaving:
                        r = rpc(sock, {"op": "deregister", "agent": agent["id"]})
                        assert r["type"] != "error", r
                    half_done = True
                    result["half_time_at"] = round(now - started, 1)
                if now >= next_prune:
                    r = rpc(sock, {"op": "checkpoint_prune", "older_than_secs": 30})
                    assert r["type"] != "error", r
                    next_prune = now + 60
                if errors:
                    break
                time.sleep(.5)
            stop.set()
            for t in threads.values():
                t.join(timeout=60)
            assert not errors, errors[0]
            assert not any(t.is_alive() for t in threads.values()), "a worker did not stop"

            # Retention did its work by itself, inside its window.
            retention_prunes = [p for p in pruned if p[1] == "retention"]
            assert retention_prunes, f"retention never pruned; prunes seen: {pruned}"
            count, oldest, cps = journal_rows(home)
            from datetime import datetime, timezone
            age = (datetime.now(timezone.utc) - datetime.fromisoformat(oldest.replace("Z", "+00:00"))).total_seconds() if oldest else 0
            retention_secs = parse_seconds(args.retention)
            assert age <= retention_secs + 75, f"oldest journal entry is {age:.0f}s old against a {retention_secs}s window"
            result["scenarios"].append(f"retention pruned the journal {len(retention_prunes)} times by itself ({sum(p[2] for p in retention_prunes)} entries); the oldest retained entry is {age:.0f}s old against a {retention_secs}s window")
            assert not read_errors, read_errors[:3]
            result["scenarios"].append(f"a reader with a cursor from before the first prune read {len(heads) - 1} pages in order; head_seq rose from {heads[0]} to {heads[-1]} and never fell")

            # Checkpoints: the finished agents' old ones went, the live ones stayed.
            for agent in staying:
                listed = rpc(sock, {"op": "checkpoints", "agent": agent["id"]})
                assert listed["type"] == "checkpoints", listed
                assert len(listed["checkpoints"]) == len(checkpoint_keys[agent["id"]]), (agent["spec"]["name"], len(listed["checkpoints"]), len(checkpoint_keys[agent["id"]]))
            gone = sum(len(checkpoint_keys[a["id"]]) for a in leaving)
            assert gone == 0 or checkpoints_pruned, "finished agents' checkpoints were never pruned"
            result["scenarios"].append(f"checkpoint pruning removed {len(checkpoints_pruned)} checkpoints of the {len(leaving)} finished agents and kept every one of the {len(staying)} live agents'")

            # Growth stops once retention holds: compare the last third with the middle third.
            samples = [s for s in result["samples"] if s["prunes_seen"] > 0]
            assert len(samples) >= 6, "too few samples after retention took hold"
            third = len(samples) // 3
            middle = samples[third:2 * third]
            last = samples[2 * third:]
            avg = lambda rows, key: sum(r[key] for r in rows) / len(rows)  # noqa: E731
            db_growth = avg(last, "db_bytes") / max(avg(middle, "db_bytes"), 1)
            rss_growth = avg(last, "rss_kib") / max(avg(middle, "rss_kib"), 1)
            result["growth"] = {"db_last_over_middle": round(db_growth, 3), "rss_last_over_middle": round(rss_growth, 3),
                                "journal_rows_middle": round(avg(middle, "journal_rows")), "journal_rows_last": round(avg(last, "journal_rows"))}
            assert db_growth < 1.5, f"database kept growing: {db_growth:.2f}x"
            assert rss_growth < 1.3, f"daemon memory kept growing: {rss_growth:.2f}x"
            result["scenarios"].append(f"after retention took hold the database averaged {db_growth:.2f}x and the daemon's memory {rss_growth:.2f}x of the middle third; journal rows {result['growth']['journal_rows_middle']} -> {result['growth']['journal_rows_last']}")

            before = db_bytes(home)
            vacuumed = rpc(sock, {"op": "vacuum", "force": True}, timeout=120)
            assert vacuumed["type"] == "vacuumed", vacuumed
            result["vacuum"] = {"before_bytes": vacuumed["before_bytes"], "after_bytes": vacuumed["after_bytes"], "db_bytes_before": before}
            result["scenarios"].append(f"vacuum reclaimed {vacuumed['before_bytes'] - vacuumed['after_bytes']} bytes")
            result["cycles"] = cycles[0]
            result["pings_ms_max"] = max(s["ping_ms"] for s in result["samples"])
            result["passed"] = True
        except Exception:  # noqa: BLE001 - the record must say what failed
            result["error"] = traceback.format_exc()
            result["passed"] = False
        finally:
            stop.set()
            try:
                rpc(sock, {"op": "shutdown"})
            except OSError:
                pass
            deadline = time.monotonic() + 15
            while sock.exists() and time.monotonic() < deadline:
                time.sleep(.05)
            if daemon.poll() is None:
                daemon.kill()
            daemon.wait(timeout=10)
            log.close()
            warnings = {}
            import re
            for line in (args.output / "daemon.log").read_text(errors="replace").splitlines():
                line = re.sub(r"\x1b\[[0-9;]*m", "", line)
                for level in (" WARN ", " ERROR "):
                    if level in line:
                        key = re.sub(r" (agent|project)=\S+", "", line.split(level, 1)[1]).strip()
                        warnings[key] = warnings.get(key, 0) + 1
            result["daemon_log_warnings"] = warnings
    (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k != "samples"}, indent=2))
    return 0 if result["passed"] else 1


def parse_seconds(text):
    digits = "".join(c for c in text if c.isdigit())
    unit = text[len(digits):].strip() or "s"
    return int(digits) * {"s": 1, "m": 60, "h": 3600, "d": 86400}[unit[0]]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--seconds", type=int, default=1200)
    parser.add_argument("--agents", type=int, default=10)
    parser.add_argument("--retention", default="90s", help="journal retention window written to agentd.toml")
    parser.add_argument("--pause", type=float, default=0.2, help="seconds between a worker's cycles")
    parser.add_argument("--sample", type=float, default=10, help="seconds between samples")
    sys.exit(trial(parser.parse_args()))


if __name__ == "__main__":
    main()
