// AgentDocker for OpenCode: leases on edits, peer messages, the project
// journal, and a wake-up when a message arrives while the session is idle.
//
// Written by `agentdocker setup opencode`. Each OpenCode event is handed to
// `agentdocker hook opencode` in Claude Code's hook shape, so the rules are
// the same ones Claude Code sessions follow: an edit to a file another agent
// holds is refused with the holder and its note, reads are recorded for
// staleness, messages reach the model at the next turn, and the turn's end
// gives back what was taken for it. If AgentDocker cannot be reached, OpenCode
// carries on as if this plugin were not here.
//
// A message leaves AgentDocker's queue only after the turn that carried it
// completed: the hook lists what its answer contains, and this plugin reports
// those back (`Delivered`) at the next `session.idle`. A turn that fails, or
// a wake-up OpenCode refuses, reports nothing, so its messages are offered
// again.
import { spawn, spawnSync } from "node:child_process"

const AGENTDOCKER = "__AGENTDOCKER__"

// OpenCode's tools, as the hook knows them, and the input it reads.
const TOOLS = {
  edit: (args) => ["Edit", { file_path: args.filePath }],
  write: (args) => ["Write", { file_path: args.filePath }],
  read: (args) => ["Read", { file_path: args.filePath }],
  apply_patch: (args) => ["apply_patch", { command: args.patchText }],
}

function hook(event) {
  const run = spawnSync(AGENTDOCKER, ["hook", "opencode"], {
    input: JSON.stringify(event),
    encoding: "utf8",
    timeout: 5000,
  })
  if (run.status !== 0 || !run.stdout || !run.stdout.trim()) return null
  try {
    return JSON.parse(run.stdout)
  } catch {
    return null
  }
}

function context(answer) {
  return answer?.hookSpecificOutput?.additionalContext ?? null
}

// The messages an answer carries: `{agent, messages: [ids]}` entries.
function carried(answer) {
  const delivered = answer?.agentdocker?.delivered
  return Array.isArray(delivered) ? delivered : []
}

function ids(entries) {
  return entries.flatMap((entry) => entry.messages ?? [])
}

export const AgentDocker = async ({ directory, client }) => {
  // What each session is told on every call (who it is, who else is here,
  // the journal); context waiting for the next request; what requests
  // carried whose turn has not completed; the session's agent; and the
  // watch that wakes an idle session.
  const orientation = new Map()
  const pending = new Map()
  const inflight = new Map()
  const agents = new Map()
  const watches = new Map()
  // Sessions idle since their last turn, a wake-up being submitted, and the
  // pending restart of a watch that ended while its session stayed idle.
  const idle = new Set()
  const waking = new Set()
  const retries = new Map()
  const base = (session, name) => ({ hook_event_name: name, session_id: session, cwd: directory })

  // Keep an answer's context until a request carries it. A message can be
  // offered again before it is acknowledged; it is told once.
  const queue = (session, answer) => {
    const text = context(answer)
    // Only messages whose text goes to the model can be acknowledged.
    const entries = text ? carried(answer) : []
    if (!text) return
    const waiting = pending.get(session) ?? []
    const known = new Set([
      ...ids(waiting.flatMap((w) => w.entries)),
      ...ids(inflight.get(session) ?? []),
    ])
    if (entries.length && ids(entries).every((id) => known.has(id))) return
    pending.set(session, [...waiting, { text, entries }])
  }

  const learn = (session, answer) => {
    const agent = answer?.agentdocker?.agent
    if (agent) agents.set(session, agent)
  }

  const deliver = (session) => {
    const done = inflight.get(session)
    inflight.delete(session)
    if (done?.length) hook({ ...base(session, "Delivered"), delivered: done })
  }

  const stopWatch = (session) => {
    const child = watches.get(session)
    watches.delete(session)
    child?.kill()
    clearTimeout(retries.get(session)?.timer)
    retries.delete(session)
  }

  // A turn is starting: nothing wakes the session until it is idle again.
  const busy = (session) => {
    idle.delete(session)
    stopWatch(session)
  }

  // Resume an idle session with what is waiting for it. One wake-up at a
  // time per session: the watch and its catch-up can both notice the same
  // message. What the prompt carries is only submitted until OpenCode
  // accepts it: a turn that ends meanwhile does not acknowledge it, and a
  // refusal drops it (the messages stay queued). Accepted, it is in flight
  // and acknowledged when a turn after that completes.
  const wake = async (session) => {
    if (!idle.has(session) || waking.has(session)) return false
    waking.add(session)
    try {
      const answer = hook({ ...base(session, "Stop"), stop_hook_active: false })
      learn(session, answer)
      if (answer?.decision !== "block" || !answer.reason) return false
      busy(session)
      const entries = carried(answer)
      try {
        // The SDK resolves a refused request with `{ error }` unless asked to
        // throw; either way a refusal is not an acceptance.
        const result = await client.session.promptAsync({
          path: { id: session },
          body: { parts: [{ type: "text", text: answer.reason }] },
          throwOnError: true,
        })
        if (result?.error) throw result.error
        inflight.set(session, [...(inflight.get(session) ?? []), ...entries])
        return true
      } catch {
        idle.add(session)
        watch(session)
        return false
      }
    } finally {
      waking.delete(session)
    }
  }

  // While idle, a message addressed to this session's agent wakes it: the
  // watch prints a line for each message that arrives, and one is enough. A
  // watch that ends while the session is still idle (the daemon restarted,
  // say) is started again, waiting longer each time up to half a minute.
  const watch = (session, delay = 1000) => {
    const agent = agents.get(session)
    if (!agent || !idle.has(session) || watches.has(session)) return
    const child = spawn(AGENTDOCKER, ["watch", "--as", agent], {
      stdio: ["ignore", "pipe", "ignore"],
    })
    watches.set(session, child)
    const ended = () => {
      if (watches.get(session) !== child) return
      watches.delete(session)
      if (!idle.has(session) || retries.has(session)) return
      const timer = setTimeout(() => {
        retries.delete(session)
        watch(session, Math.min(delay * 2, 30000))
      }, delay)
      retries.set(session, { timer })
    }
    child.on("error", ended)
    child.on("exit", ended)
    child.stdout.on("data", () => {
      if (watches.get(session) === child) wake(session)
    })
    // A message that arrived while the watch was starting is already queued.
    setTimeout(() => {
      if (watches.get(session) === child) wake(session)
    }, 1000)
  }

  return {
    "tool.execute.before": async (input, output) => {
      busy(input.sessionID)
      const map = TOOLS[input.tool]
      if (!map) return
      const [tool_name, tool_input] = map(output.args ?? {})
      const answer = hook({ ...base(input.sessionID, "PreToolUse"), tool_name, tool_input })
      const decision = answer?.hookSpecificOutput
      if (decision?.permissionDecision === "deny") {
        // Throwing refuses the tool call; the model reads why.
        throw new Error(decision.permissionDecisionReason)
      }
    },

    "tool.execute.after": async (input) => {
      const map = TOOLS[input.tool]
      const [tool_name, tool_input] = map ? map(input.args ?? {}) : [input.tool, {}]
      queue(input.sessionID, hook({ ...base(input.sessionID, "PostToolUse"), tool_name, tool_input }))
    },

    // Every message the person sends starts a turn: messages that arrived
    // since the last one go with it.
    "chat.message": async (input) => {
      const session = input.sessionID
      if (!session) return
      busy(session)
      queue(session, hook(base(session, "UserPromptSubmit")))
    },

    "experimental.chat.system.transform": async (input, output) => {
      const session = input.sessionID
      if (!session) return
      if (!orientation.has(session)) {
        const answer = hook(base(session, "SessionStart"))
        learn(session, answer)
        // Orientation is repeated on every request; the messages waiting at
        // the start are told once, like any other delivery.
        const start = context(answer) ?? ""
        const cut = start.indexOf("\nMessages waiting (")
        orientation.set(session, cut < 0 ? start : start.slice(0, cut))
        queue(session, {
          hookSpecificOutput: { additionalContext: cut < 0 ? null : start.slice(cut + 1) },
          agentdocker: answer?.agentdocker,
        })
      }
      const told = orientation.get(session)
      if (told) output.system.push(told)
      const waiting = pending.get(session)
      if (waiting?.length) {
        const text = waiting
          .map((w) => w.text)
          .filter(Boolean)
          .join("\n\n")
        if (text) output.system.push(text)
        pending.delete(session)
        // In a request now; acknowledged once its turn completes.
        inflight.set(session, [...(inflight.get(session) ?? []), ...waiting.flatMap((w) => w.entries)])
      }
    },

    event: async ({ event }) => {
      const session = event?.properties?.sessionID ?? event?.properties?.info?.id
      if (!session) return
      if (event.type === "session.idle") {
        // The turn completed, so what it carried was delivered. Then the
        // turn's end gives automatic leases back, and waiting messages wake
        // the session at once or, when none are waiting, when one arrives.
        deliver(session)
        idle.add(session)
        if (!(await wake(session))) watch(session)
      } else if (event.type === "session.error") {
        // The turn did not complete: nothing it carried is acknowledged,
        // so AgentDocker offers those messages again.
        inflight.delete(session)
      } else if (event.type === "session.deleted") {
        busy(session)
        hook(base(session, "SessionEnd"))
        for (const map of [orientation, pending, inflight, agents]) map.delete(session)
      }
    },
  }
}
