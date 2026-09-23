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
import { spawnSync } from "node:child_process"

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

export const AgentDocker = async ({ directory, client }) => {
  // What each session is told on every call (who it is, who else is here,
  // the journal), and messages waiting to be told once.
  const orientation = new Map()
  const pending = new Map()
  const queue = (session, text) => {
    if (!text) return
    pending.set(session, [...(pending.get(session) ?? []), text])
  }
  const base = (session, name) => ({ hook_event_name: name, session_id: session, cwd: directory })

  return {
    "tool.execute.before": async (input, output) => {
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
      queue(input.sessionID, context(hook({ ...base(input.sessionID, "PostToolUse"), tool_name, tool_input })))
    },

    "experimental.chat.system.transform": async (input, output) => {
      const session = input.sessionID
      if (!session) return
      if (!orientation.has(session)) {
        orientation.set(session, context(hook(base(session, "SessionStart"))) ?? "")
        queue(session, context(hook(base(session, "UserPromptSubmit"))))
      }
      const told = orientation.get(session)
      if (told) output.system.push(told)
      const waiting = pending.get(session)
      if (waiting?.length) {
        output.system.push(waiting.join("\n\n"))
        pending.delete(session)
      }
    },

    event: async ({ event }) => {
      const session = event?.properties?.sessionID ?? event?.properties?.info?.id
      if (!session) return
      if (event.type === "session.idle") {
        // The turn's end: automatic leases go back. If messages are waiting
        // the hook says so, and the session is woken with them.
        const answer = hook({ ...base(session, "Stop"), stop_hook_active: false })
        if (answer?.decision === "block" && answer.reason) {
          try {
            await client.session.promptAsync({
              path: { id: session },
              body: { parts: [{ type: "text", text: answer.reason }] },
            })
          } catch {
            // The messages stay queued for the next turn.
          }
        }
      } else if (event.type === "session.deleted") {
        hook(base(session, "SessionEnd"))
        orientation.delete(session)
        pending.delete(session)
      }
    },
  }
}
