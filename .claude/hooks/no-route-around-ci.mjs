#!/usr/bin/env node
// PreToolUse(Bash) guard: the only way to main is a pull request that CI passed.
//
// Denies, in any permission mode (PreToolUse runs before the permission check,
// so loosening permissions does not get past it):
//   - `gh pr merge` in any form, and any `gh` call carrying `--admin`
//   - `gh workflow …` and `gh variable set …` (the merge machine and its kill
//     switch are not the agent's to touch)
//   - `git push` that forces, or that targets main
//   - `git commit --no-verify` (the pre-commit hook is part of the gate)
//   - `git commit` while `.github/workflows/**` is staged (a pull request runs
//     its own copy of ci.yml, so a worker could weaken the gate in the same PR
//     the gate then approves)
//
// Everything else passes. Fails open on any error: a guard that blocks by
// accident is worse than one that misses, because the merge gate on GitHub is
// still there behind it.
//
// Repo-agnostic: nothing here names this project. Language-agnostic too.
import { execFileSync } from "node:child_process";
import { readSync } from "node:fs";

function readStdin() {
  const chunks = [];
  let n;
  const buf = Buffer.alloc(65536);
  try {
    while ((n = readSync(0, buf, 0, buf.length, null)) > 0) chunks.push(Buffer.from(buf.subarray(0, n)));
  } catch (e) {
    if (e.code !== "EAGAIN" && e.code !== "EOF") throw e;
  }
  return Buffer.concat(chunks).toString("utf8");
}

export function judge(command, stagedPaths) {
  const c = String(command || "");
  const reasons = [];
  if (/\bgh\s+pr\s+merge\b/.test(c)) reasons.push("`gh pr merge` is not a route to main; the merge gate is CI plus the repo's merge command");
  if (/\bgh\b[^\n]*\s--admin\b/.test(c)) reasons.push("`--admin` bypasses the checks the gate exists to wait on");
  if (/\bgh\s+workflow\b/.test(c)) reasons.push("`gh workflow` changes the merge machine; that is a human's change");
  if (/\bgh\s+variable\s+set\b/.test(c)) reasons.push("`gh variable set` is the kill switch; only a human flips it");
  for (const m of c.matchAll(/\bgit\s+push\b([^\n;&|]*)/g)) {
    const args = m[1];
    if (/(^|\s)(--force|--force-with-lease|-f|\+\S+)(\s|$)/.test(args) || /\s-\w*f\w*(\s|$)/.test(args)) reasons.push("force push rewrites history the gate already judged");
    const toks = args.trim().split(/\s+/).filter(Boolean);
    if (toks.some((t) => t === "main" || t === "HEAD:main" || t.endsWith(":main") || t === "refs/heads/main" || t.endsWith(":refs/heads/main"))) {
      reasons.push("pushing to main skips the pull request; branch, push, and let CI land it");
    }
  }
  if (/\bgit\s+commit\b[^\n]*\s(--no-verify|-n)(\s|$)/.test(c)) reasons.push("`--no-verify` skips the pre-commit hook, which is part of the gate");
  if (/\bgit\s+commit\b/.test(c)) {
    const wf = (stagedPaths || []).filter((p) => p.startsWith(".github/workflows/"));
    if (wf.length) reasons.push(`a commit that touches ${wf.join(", ")} can weaken the gate that judges it; a human lands workflow changes`);
  }
  return reasons;
}

function staged(cwd, command) {
  try {
    const out = execFileSync("git", ["diff", "--cached", "--name-only"], { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
    let paths = out.split("\n").filter(Boolean);
    if (/\bgit\s+commit\b[^\n]*\s(-a|--all|-am|-a\w+)(\s|$)/.test(command)) {
      const wt = execFileSync("git", ["diff", "--name-only"], { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
      paths = paths.concat(wt.split("\n").filter(Boolean));
    }
    return paths;
  } catch {
    return [];
  }
}

function main() {
  let payload;
  try {
    payload = JSON.parse(readStdin() || "{}");
  } catch {
    return;
  }
  if (payload.tool_name !== "Bash") return;
  const command = payload.tool_input && payload.tool_input.command;
  if (!command) return;
  const reasons = judge(command, /\bgit\s+commit\b/.test(command) ? staged(payload.cwd || process.cwd(), command) : []);
  if (!reasons.length) return;
  process.stdout.write(JSON.stringify({
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason: `no-route-around-ci: ${reasons.join("; ")}.`,
    },
  }));
}

if (process.argv[1] && import.meta.url.endsWith(process.argv[1].split("/").pop())) main();
