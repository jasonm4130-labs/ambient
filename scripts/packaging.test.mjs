import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const repoRoot = join(import.meta.dirname, "..");
const scriptsDir = join(repoRoot, "scripts");

function tempDir(prefix) {
  return mkdtempSync(join(tmpdir(), `ambient-${prefix}-`));
}

function runScript(name, args = [], extraEnv = {}) {
  return spawnSync(join(scriptsDir, name), args, {
    cwd: repoRoot,
    encoding: "utf8",
    env: { ...process.env, ...extraEnv },
  });
}

function makeMockCargo(dir, capture) {
  const bin = join(dir, "bin");
  mkdirSync(bin);
  const cargo = join(bin, "cargo");
  writeFileSync(
    cargo,
    "#!/bin/sh\nprintf '%s\\n' \"$CARGO_ENCODED_RUSTFLAGS\" > \"$CAPTURE\"\nprintf '%s\\n' \"$@\" >> \"$CAPTURE\"\n",
  );
  chmodSync(cargo, 0o755);
  return { PATH: `${bin}:${process.env.PATH}`, CAPTURE: capture };
}

function expectedRemaps(prefix) {
  const home = process.env.HOME;
  const homeRemap = home && home !== "/" ? [`--remap-path-prefix=${home}=/home/build`] : [];
  return [...prefix, ...homeRemap, `--remap-path-prefix=${repoRoot}=/ambient`];
}

function readCapturedFlags(capture) {
  return readFileSync(capture, "utf8").split("\n")[0];
}

test("build-release preserves plain RUSTFLAGS and appends path remaps", () => {
  const dir = tempDir("build-release-plain");
  try {
    const capture = join(dir, "capture");
    const env = makeMockCargo(dir, capture);
    const result = runScript("build-release", [], {
      ...env,
      RUSTFLAGS: "--cfg feature=demo -C opt-level=2",
      CARGO_ENCODED_RUSTFLAGS: undefined,
    });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(
      readCapturedFlags(capture).split("\x1f"),
      expectedRemaps(["--cfg", "feature=demo", "-C", "opt-level=2"]),
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("build-release preserves existing encoded flags", () => {
  const dir = tempDir("build-release-encoded");
  try {
    const capture = join(dir, "capture");
    const env = makeMockCargo(dir, capture);
    const result = runScript("build-release", [], {
      ...env,
      RUSTFLAGS: "--cfg should_not_be_used",
      CARGO_ENCODED_RUSTFLAGS: "-C\x1fdebuginfo=1",
    });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(
      readCapturedFlags(capture).split("\x1f"),
      expectedRemaps(["-C", "debuginfo=1"]),
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("build-release gives encoded flags precedence over RUSTFLAGS", () => {
  const dir = tempDir("build-release-precedence");
  try {
    const capture = join(dir, "capture");
    const env = makeMockCargo(dir, capture);
    const result = runScript("build-release", [], {
      ...env,
      RUSTFLAGS: "--cfg plain_flag",
      CARGO_ENCODED_RUSTFLAGS: "--cfg\x1fencoded_flag",
    });
    assert.equal(result.status, 0, result.stderr);
    const flags = readCapturedFlags(capture).split("\x1f");
    assert.deepEqual(flags, expectedRemaps(["--cfg", "encoded_flag"]));
    assert.equal(flags.includes("plain_flag"), false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

function makeBundle(dir, executable = "synthetic executable") {
  const app = join(dir, "Ambient.app");
  const resources = join(app, "Contents", "Resources");
  mkdirSync(join(app, "Contents", "MacOS"), { recursive: true });
  mkdirSync(resources, { recursive: true });
  const binary = join(app, "Contents", "MacOS", "ambient");
  writeFileSync(binary, executable);
  chmodSync(binary, 0o755);
  cpSync(join(repoRoot, "LICENSE"), join(resources, "LICENSE"));
  cpSync(join(repoRoot, "THIRD_PARTY_NOTICES.md"), join(resources, "THIRD_PARTY_NOTICES.md"));
  cpSync(join(repoRoot, "licenses"), join(resources, "licenses"), { recursive: true });
  return { app, resources };
}

test("check-bundle accepts a valid model-free payload", () => {
  const dir = tempDir("check-bundle-valid");
  try {
    const { app } = makeBundle(dir);
    const result = runScript("check-bundle", [app]);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /bundle payload: verified/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("check-bundle rejects a missing or stale notice", () => {
  for (const mode of ["missing", "stale"]) {
    const dir = tempDir(`check-bundle-${mode}`);
    try {
      const { resources, app } = makeBundle(dir);
      const notice = join(resources, "THIRD_PARTY_NOTICES.md");
      if (mode === "missing") rmSync(notice);
      else writeFileSync(notice, "stale notice\n");
      const result = runScript("check-bundle", [app]);
      assert.notEqual(result.status, 0, `${mode} notice unexpectedly accepted`);
      assert.match(result.stderr, /THIRD_PARTY_NOTICES\.md/);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  }
});

test("check-bundle rejects the owner home path in the executable", () => {
  const dir = tempDir("check-bundle-home");
  try {
    assert.ok(process.env.HOME && process.env.HOME !== "/");
    const { app } = makeBundle(dir, `binary built at ${process.env.HOME}/Work/Git/ambient\n`);
    const result = runScript("check-bundle", [app]);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /build user's home path/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("check-bundle --models rejects a payload with no models", () => {
  const dir = tempDir("check-bundle-models");
  try {
    const { app } = makeBundle(dir);
    const result = runScript("check-bundle", [app, "--models"]);
    assert.notEqual(result.status, 0);
    assert.match(`${result.stdout}\n${result.stderr}`, /No such file|models/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});


test("build path check allows only the exact precompiled ONNX runner prefix", () => {
  const dir = tempDir("upstream-paths");
  try {
    const binary = join(dir, "binary");
    const upstream = "/Users/runner/work/ort-artifacts/ort-artifacts/onnxruntime/core.cc";
    writeFileSync(binary, `${upstream}\0`);
    assert.equal(runScript("check-build-paths", [binary, "/Users/runner"]).status, 0);
    for (const leaked of ["/Users/runner/_work/ambient/src/main.rs", "/Users/runner/.cargo/registry/crate.rs"]) {
      writeFileSync(binary, `${upstream} ${leaked}\0`);
      assert.notEqual(runScript("check-build-paths", [binary, "/Users/runner"]).status, 0);
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
