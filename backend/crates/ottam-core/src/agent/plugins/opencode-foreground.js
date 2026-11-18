// Ottam's OpenCode plugin: keep every shell command in the foreground, and
// give shell commands the user's own environment.
//
// A background shell command reports back by resuming the session on its
// own, a turn Ottam can't tell from the agent's own work. Stripping
// `background` keeps the command, and its result, inside the turn that asked
// for it. Subagents always run in the background; core threads them from
// `opencode/session/child_update`.
//
// An Ottam profile points OpenCode's homes, XDG ones included, inside the
// profile. Shell commands are the user's, so they get the user's values back.
import { Plugin } from "@opencode/plugin"

const SHELL_ENVIRONMENT = "OTTAM_SHELL_ENVIRONMENT"

function userEnvironment() {
  try {
    const own = JSON.parse(process.env[SHELL_ENVIRONMENT] ?? "{}")
    return own && typeof own === "object" ? own : {}
  } catch {
    return {}
  }
}

export default Plugin.define({
  id: "ottam.foreground",
  async setup(ctx) {
    const own = userEnvironment()
    await ctx.tool.hook("execute.before", (event) => {
      if (event.tool === "shell" && event.input && typeof event.input === "object") {
        delete event.input.background
      }
    })
    await ctx.shell.hook("create.before", (event) => {
      delete event.env[SHELL_ENVIRONMENT]
      for (const [name, value] of Object.entries(own)) {
        if (typeof value === "string") event.env[name] = value
        else delete event.env[name]
      }
    })
    await ctx.session.hook("context", (event) => {
      event.system.push({
        type: "text",
        text: "Background shell commands are unavailable in this session. Run shell commands in the foreground and wait for their results.",
      })
    })
  },
})
