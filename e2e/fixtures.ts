import { test as base, expect } from "@playwright/test";
import { spawn, spawnSync, execFileSync } from "node:child_process";
import { once } from "node:events";
import { chmod, copyFile, mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";

interface App {
  directory: string;
  skillRoot: string;
  otherSkillRoot: string;
  binDirectory: string;
  baseURL: string;
}

// Each test owns its files, config, backend and tmux socket. Never inherit agent
// credentials or touch the developer's detached host / phone-access mapping.
export const test = base.extend<{ app: App }>({
  // Playwright requires a destructured dependency argument for fixtures.
  // eslint-disable-next-line no-empty-pattern
  app: async ({}, use, testInfo) => {
    const tmux = execFileSync("which", ["tmux"], { encoding: "utf8" }).trim();
    const directory = await mkdtemp("/tmp/vs-e2e-");
    const binDirectory = join(directory, "bin");
    const fixtureHome = join(directory, "home");
    const skillRoot = join(fixtureHome, ".agents/skills/e2e-primary");
    const otherSkillRoot = join(fixtureHome, ".agents/skills/e2e-secondary");
    const socket = join(directory, "tmux", `tmux-${process.getuid!()}`, "default");
    const env = {
      HOME: fixtureHome,
      XDG_CONFIG_HOME: join(directory, "config"),
      XDG_CACHE_HOME: join(directory, "cache"),
      XDG_DATA_HOME: join(directory, "data"),
      TMUX_TMPDIR: join(directory, "tmux"),
      TMPDIR: join(directory, "tmp"),
      PATH: `${binDirectory}:/usr/bin:/bin`,
      SHELL: "/bin/bash",
      TERM: "xterm-256color",
      LANG: "en_US.UTF-8",
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_CONFIG_GLOBAL: "/dev/null",
      GIT_AUTHOR_NAME: "E2E",
      GIT_AUTHOR_EMAIL: "e2e@example.invalid",
      GIT_COMMITTER_NAME: "E2E",
      GIT_COMMITTER_EMAIL: "e2e@example.invalid",
    };
    let server: ReturnType<typeof spawn> | undefined;
    let log = "";
    try {
      for (const path of [binDirectory, skillRoot, otherSkillRoot, env.XDG_CONFIG_HOME,
        env.XDG_CACHE_HOME, env.XDG_DATA_HOME, env.TMUX_TMPDIR, env.TMPDIR]) {
        await mkdir(path, { recursive: true });
      }
      await symlink(tmux, join(binDirectory, "tmux"));
      await copyFile("e2e/codex.sh", join(binDirectory, "codex"));
      await chmod(join(binDirectory, "codex"), 0o700);
      for (const [root, name, description] of [
        [skillRoot, "e2e-primary", "Primary E2E skill"],
        [otherSkillRoot, "e2e-secondary", "Secondary E2E skill"],
      ]) {
        await writeFile(join(root, "SKILL.md"), `---\nname: ${name}\ndescription: ${description}\n---\n\n# Instructions\n\nA disposable test skill.\n`);
      }
      await writeFile(join(skillRoot, "notes.md"), "# Notes\n\nOriginal note.\n");
      server = spawn(resolve("target/debug/skill-server"), [
        "--port", "0", "--dist", resolve("dist"),
        "--no-startup-maintenance", "--lifeline-stdin",
      ], { cwd: directory, env, stdio: "pipe" });
      const child = server;
      const baseURL = await new Promise<string>((resolveReady, reject) => {
        const timer = setTimeout(() => reject(new Error(`Backend did not start:\n${log}`)), 10_000);
        const fail = (error: Error) => { clearTimeout(timer); reject(error); };
        child.once("error", fail);
        child.once("exit", (code) => fail(new Error(`Backend exited (${code}):\n${log}`)));
        child.stderr!.on("data", (chunk) => { log += chunk; });
        child.stdout!.on("data", (chunk) => {
          log += chunk;
          const ready = log.match(/^SKILL_SERVER_READY port=(\d+)$/m);
          if (ready) { clearTimeout(timer); resolveReady(`http://127.0.0.1:${ready[1]}`); }
        });
      });
      await use({ directory, skillRoot, otherSkillRoot, binDirectory, baseURL });
    } finally {
      if (server && server.exitCode === null && server.signalCode === null) {
        const stopped = once(server, "close");
        server.stdin?.end();
        server.kill("SIGTERM");
        const force = setTimeout(() => server?.kill("SIGKILL"), 3_000);
        await stopped;
        clearTimeout(force);
      }
      // Explicit private socket: never fall back to a live user's tmux server.
      spawnSync(tmux, ["-S", socket, "kill-server"], { env, timeout: 5_000 });
      if (testInfo.status !== testInfo.expectedStatus) {
        const logPath = testInfo.outputPath("backend.log");
        await writeFile(logPath, log);
        await testInfo.attach("backend.log", { path: logPath, contentType: "text/plain" });
      }
      await rm(directory, { recursive: true, force: true });
    }
  },
  baseURL: async ({ app }, use) => { await use(app.baseURL); },
});

export { expect };
