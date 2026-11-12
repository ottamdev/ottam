// Ottam's OpenCode plugin: keep every shell command and subagent in the
// foreground, and give shell commands the user's own environment.
//
// OpenCode resumes a session on its own when background work finishes, but
// its ACP layer drops that resumed turn after the prompt response, so
// Ottam would never show it. Stripping `background` keeps the work, and
// its result, inside the turn that asked for it.
//
// An Ottam profile points OpenCode's homes, XDG ones included, inside the
// profile. Shell commands are the user's, so they get the user's values back.
import { Plugin } from "@opencode/plugin"

const FOREGROUND_TOOLS = new Set(["shell", "subagent"])
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
      if (FOREGROUND_TOOLS.has(event.tool) && event.input && typeof event.input === "object") {
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
        text: "Background execution is unavailable in this session. Run shell commands and subagents in the foreground and wait for their results.",
      })
    })
  },
})
