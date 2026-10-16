#!/usr/bin/env node
// PreToolUse(Bash) guard: a commit may add tests, never take them away.
//
// On `git commit`, reads the staged diff (plus the working-tree diff for
// `-a`) and denies when the commit removes more test markers than it adds, or
// deletes a file whose name says it is a test. The markers are per language,
// so the same hook serves any repo:
//   Rust    #[test]  #[cfg(test)]  #[tokio::test]
//   JS/TS   test(  it(  describe(
//   Python  def test_
//   Go      func Test
//
// A heuristic, not a proof: a determined agent can restructure around it. It
// exists because METR measured frontier models attempting to reward-hack in
// ~80% of attempts when tests were hidden, and Kent Beck reports agents
// deleting tests to make them pass. A wrong test blocks the task instead —
// say so in the report and let a human read it.
//
// Fails open on any error.
import { execFileSync } from "node:child_process";
import { readSync } from "node:fs";

const MARKERS = [
  /#\[(tokio::)?test\]/, /#\[cfg\(test\)\]/,
  /^\s*(test|it|describe)\s*\(/,
  /^\s*(async\s+)?def\s+test_/,
  /^\s*func\s+Test[A-Z_]/,
];
// The git-invocation pattern from no-route-around-ci.mjs, with its global
// options captured: `git -C dir commit` and `git --git-dir=x commit` are
// commits too, and the diff must be read from the repo they name.
const GIT = String.raw`(?:^|[\s;&|(\`{])(?:(?:command|exec|builtin)\s+(?:-\w+\s+)*|env\s+(?:\w+=\S*\s+)*|\\|[^\s;&|"']*/)?["']?git["']?\s+((?:(?:-[A-Za-z]|--[\w-]+)(?:[=\s]+[^\s;&|]+)?\s+)*)`;
const COMMIT = new RegExp(GIT + String.raw`commit\b([^\n;&|]*)`);

// The global options that choose the repo, as argv for the diff calls.
export function repoArgs(opts) {
  const toks = String(opts || "").trim().split(/\s+/).filter(Boolean).map((t) => t.replace(/^["']|["']$/g, ""));
  const out = [];
  for (let i = 0; i < toks.length; i++) {
    const t = toks[i];
    if (t === "-C" && toks[i + 1]) out.push("-C", toks[++i]);
    else if (/^--(git-dir|work-tree)=/.test(t)) out.push(t);
    else if (/^--(git-dir|work-tree)$/.test(t) && toks[i + 1]) out.push(`${t}=${toks[++i]}`);
  }
  return out;
}

const TEST_PATH = /(^|\/)(tests?|__tests__|spec)\/|[._-]test\.[a-z]+$|_test\.(rs|go|py)$|\.spec\.[a-z]+$/;

function readStdin() {
  const chunks = [];
  const buf = Buffer.alloc(65536);
  let n;
  try {
    while ((n = readSync(0, buf, 0, buf.length, null)) > 0) chunks.push(Buffer.from(buf.subarray(0, n)));
  } catch (e) {
    if (e.code !== "EAGAIN" && e.code !== "EOF") throw e;
  }
  return Buffer.concat(chunks).toString("utf8");
}

export function judge(diff, deletedPaths) {
  let removed = 0;
  let added = 0;
  for (const line of String(diff || "").split("\n")) {
    if (line.startsWith("+++") || line.startsWith("---")) continue;
    const sign = line[0];
    if (sign !== "+" && sign !== "-") continue;
    const body = line.slice(1);
    if (MARKERS.some((m) => m.test(body))) {
      if (sign === "-") removed++;
      else added++;
    }
  }
  const reasons = [];
  if (removed > added) reasons.push(`the commit removes ${removed} test marker(s) and adds ${added}; tests are read-only to the loop — if a test is wrong, block the task and say why`);
  const del = (deletedPaths || []).filter((p) => TEST_PATH.test(p));
  if (del.length) reasons.push(`the commit deletes test file(s): ${del.join(", ")}`);
  return reasons;
}

function gitDiff(cwd, args) {
  try {
    return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], maxBuffer: 64 * 1024 * 1024 });
  } catch {
    return "";
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
  const command = String((payload.tool_input && payload.tool_input.command) || "");
  const m = command.match(COMMIT);
  if (!m) return;
  const cwd = payload.cwd || process.cwd();
  const pre = repoArgs(m[1]);
  const all = /(^|\s)(-a|--all|-am|-a\w+)(\s|$)/.test(m[2]);
  let diff = gitDiff(cwd, [...pre, "diff", "--cached"]);
  let status = gitDiff(cwd, [...pre, "diff", "--cached", "--name-status"]);
  if (all) {
    diff += gitDiff(cwd, [...pre, "diff"]);
    status += gitDiff(cwd, [...pre, "diff", "--name-status"]);
  }
  const deleted = status.split("\n").filter((l) => l.startsWith("D\t")).map((l) => l.split("\t")[1]);
  const reasons = judge(diff, deleted);
  if (!reasons.length) return;
  process.stdout.write(JSON.stringify({
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason: `tests-are-readonly: ${reasons.join("; ")}.`,
    },
  }));
}

if (process.argv[1] && import.meta.url.endsWith(process.argv[1].split("/").pop())) main();
