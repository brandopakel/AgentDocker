// Drives the OpenCode plugin as shipped, with its child processes, timers and
// OpenCode client replaced, through the paths a real session cannot be made
// to take on demand: a hook that answers without text, overlapping wake-ups,
// a watch that ends, and a prompt OpenCode refuses. Run by
// tests/test_opencode_plugin.py.
const fs = require("node:fs")
const path = require("node:path")
const vm = require("node:vm")
const { EventEmitter } = require("node:events")
const assert = require("node:assert/strict")

const source = fs
  .readFileSync(path.join(__dirname, "../crates/cli/opencode/agentdocker.js"), "utf8")
  .replace('import { spawn, spawnSync } from "node:child_process"', "")
  .replace("export const AgentDocker =", "globalThis.AgentDocker =")

const M = [{ agent: "agent-a", messages: ["message-m"] }]
const orientation = {
  hookSpecificOutput: { additionalContext: "Orientation." },
  agentdocker: { agent: "agent-a", delivered: [] },
}

// `answers(event)` gives the hook's answer to each event; `prompt()` is
// what client.session.promptAsync returns.
async function start({ answers, prompt }) {
  const calls = []
  const prompts = []
  const timers = []
  const children = []
  const sandbox = {
    setTimeout(fn, delay) {
      const timer = { fn, delay, done: false }
      timers.push(timer)
      return timer
    },
    clearTimeout(timer) {
      if (timer) timer.done = true
    },
    spawn() {
      const child = new EventEmitter()
      child.stdout = new EventEmitter()
      child.kill = () => child.emit("exit", null)
      children.push(child)
      return child
    },
    spawnSync(_binary, _args, options) {
      const event = JSON.parse(options.input)
      calls.push(event)
      const answer = event.hook_event_name === "SessionStart" ? orientation : answers(event) ?? {}
      return { status: 0, stdout: JSON.stringify(answer) }
    },
  }
  vm.createContext(sandbox)
  vm.runInContext(source, sandbox)
  const client = {
    session: {
      promptAsync(options) {
        prompts.push(options)
        return prompt()
      },
    },
  }
  const plugin = await sandbox.AgentDocker({ directory: "/project", client })
  const run = async () => {
    for (const timer of timers.splice(0)) {
      if (!timer.done) {
        timer.done = true
        timer.fn()
      }
    }
    await new Promise((resolve) => setImmediate(resolve))
  }
  const event = (type) => plugin.event({ event: { type, properties: { sessionID: "s" } } })
  const delivered = () => calls.filter((c) => c.hook_event_name === "Delivered")
  // A first turn, so the session has its agent and orientation.
  await plugin["experimental.chat.system.transform"]({ sessionID: "s" }, { system: [] })
  return { plugin, calls, prompts, timers, children, run, event, delivered }
}

const tests = {
  // A hook that read the inbox but produced no text holds no receipt.
  async "a message whose text never reached the model is not acknowledged"() {
    const t = await start({
      answers: (e) => (e.hook_event_name === "UserPromptSubmit" ? { agentdocker: { delivered: M } } : null),
      prompt: () => Promise.resolve(),
    })
    await t.plugin["chat.message"]({ sessionID: "s" })
    const output = { system: [] }
    await t.plugin["experimental.chat.system.transform"]({ sessionID: "s" }, output)
    await t.event("session.idle")
    assert.deepEqual(Array.from(output.system), ["Orientation."])
    assert.equal(t.delivered().length, 0)
  },

  // The watch's line and its one-second catch-up notice the same message
  // while the first prompt is still being submitted: one prompt goes out.
  async "overlapping wake-ups submit one prompt"() {
    let queued = false
    let accept
    const t = await start({
      answers: (e) =>
        e.hook_event_name === "Stop" && queued
          ? { decision: "block", reason: "Message M", agentdocker: { agent: "agent-a", delivered: M } }
          : null,
      prompt: () => new Promise((resolve) => (accept = resolve)),
    })
    await t.event("session.idle")
    assert.equal(t.children.length, 1)
    queued = true
    // The catch-up fires first and its prompt is still being submitted when
    // the watch prints the same message.
    await t.run()
    assert.equal(t.prompts.length, 1)
    t.children[0].stdout.emit("data", "message\n")
    await t.run()
    assert.equal(t.prompts.length, 1)
    accept()
    await t.run()
    assert.equal(t.prompts.length, 1)
    // The woken turn completes: its message is acknowledged once, and the
    // queue is empty afterwards.
    queued = false
    await t.event("session.idle")
    assert.equal(t.delivered().length, 1)
    assert.deepEqual(Array.from(t.delivered()[0].delivered[0].messages), ["message-m"])
  },

  // A watch that ends while the session is idle (the daemon restarted) is
  // started again, later each time; a turn cancels the restart.
  async "an ended watch is started again while the session stays idle"() {
    const t = await start({ answers: () => null, prompt: () => Promise.resolve() })
    await t.event("session.idle")
    assert.equal(t.children.length, 1)
    t.children[0].emit("exit", 1)
    const retry = t.timers.find((timer) => !timer.done && timer.delay === 1000)
    assert.ok(retry, "a restart is scheduled")
    await t.run()
    assert.equal(t.children.length, 2)
    t.children[1].emit("exit", 1)
    assert.ok(t.timers.some((timer) => !timer.done && timer.delay === 2000), "it backs off")
    await t.plugin["chat.message"]({ sessionID: "s" })
    await t.run()
    assert.equal(t.children.length, 2, "a turn cancels the restart")
  },

  // OpenCode refuses the prompt: nothing is acknowledged, and the session
  // goes back to watching.
  async "a refused wake-up acknowledges nothing and keeps watching"() {
    const t = await start({
      answers: (e) =>
        e.hook_event_name === "Stop"
          ? { decision: "block", reason: "Message M", agentdocker: { agent: "agent-a", delivered: M } }
          : null,
      prompt: () => Promise.reject(new Error("busy")),
    })
    await t.event("session.idle")
    assert.equal(t.prompts.length, 1)
    assert.equal(t.children.length, 1, "watching again")
    // The watch's catch-up tries again; every refusal leaves it queued.
    await t.run()
    assert.equal(t.prompts.length, 2)
    await t.event("session.idle")
    assert.equal(t.delivered().length, 0)
  },

  // The SDK's default client resolves a refusal instead of throwing: it is
  // still a refusal, and a later turn does not acknowledge what it carried.
  async "a refusal the SDK resolves instead of throwing is still a refusal"() {
    let block = true
    const t = await start({
      answers: (e) =>
        e.hook_event_name === "Stop" && block
          ? { decision: "block", reason: "Message M", agentdocker: { agent: "agent-a", delivered: M } }
          : null,
      prompt: () => Promise.resolve({ error: { name: "BadRequest" }, response: { status: 400 } }),
    })
    await t.event("session.idle")
    assert.equal(t.prompts.length, 1)
    assert.equal(t.prompts[0].throwOnError, true)
    assert.equal(t.children.length, 1, "watching again")
    block = false
    await t.plugin["chat.message"]({ sessionID: "s" })
    await t.plugin["experimental.chat.system.transform"]({ sessionID: "s" }, { system: [] })
    await t.event("session.idle")
    assert.equal(t.delivered().length, 0)
  },

  // A turn that ends while a wake-up's prompt is still unconfirmed does not
  // acknowledge it, and a later refusal leaves it queued.
  async "a wake-up that is refused after another turn ends is not acknowledged"() {
    let refuse
    const t = await start({
      answers: (e) =>
        e.hook_event_name === "Stop"
          ? { decision: "block", reason: "Message M", agentdocker: { agent: "agent-a", delivered: M } }
          : null,
      prompt: () => new Promise((resolve) => (refuse = () => resolve({ error: { name: "BadRequest" } }))),
    })
    const woken = t.event("session.idle")
    await t.run()
    assert.equal(t.prompts.length, 1)
    await t.plugin["chat.message"]({ sessionID: "s" })
    await t.event("session.idle")
    assert.equal(t.delivered().length, 0, "the unconfirmed wake-up is not acknowledged")
    refuse()
    await woken
    await t.run()
    await t.event("session.idle")
    const acknowledged = t.delivered().flatMap((c) => c.delivered.flatMap((d) => d.messages))
    assert.ok(!acknowledged.includes("message-m"), `acknowledged ${acknowledged}`)
  },

  // Text that reached a completed turn is acknowledged; an errored turn's
  // is not.
  async "a completed turn acknowledges what it carried and an errored one does not"() {
    let answer = { hookSpecificOutput: { additionalContext: "Message M" }, agentdocker: { delivered: M } }
    const t = await start({
      answers: (e) => (e.hook_event_name === "UserPromptSubmit" ? answer : null),
      prompt: () => Promise.resolve(),
    })
    await t.plugin["chat.message"]({ sessionID: "s" })
    const output = { system: [] }
    await t.plugin["experimental.chat.system.transform"]({ sessionID: "s" }, output)
    assert.ok(output.system.includes("Message M"))
    await t.event("session.error")
    await t.event("session.idle")
    assert.equal(t.delivered().length, 0, "the errored turn acknowledged nothing")
    await t.plugin["chat.message"]({ sessionID: "s" })
    await t.plugin["experimental.chat.system.transform"]({ sessionID: "s" }, { system: [] })
    await t.event("session.idle")
    assert.equal(t.delivered().length, 1)
  },
}

// A test that stops making progress fails rather than letting Node exit.
const guard = setInterval(() => {}, 1000)
;(async () => {
  let failed = 0
  for (const [name, test] of Object.entries(tests)) {
    try {
      await Promise.race([
        test(),
        new Promise((_, reject) => setTimeout(() => reject(new Error("stalled")), 5000)),
      ])
      console.log(`ok - ${name}`)
    } catch (error) {
      failed++
      console.log(`not ok - ${name}\n${error.stack}`)
    }
  }
  clearInterval(guard)
  process.exit(failed ? 1 : 0)
})()
