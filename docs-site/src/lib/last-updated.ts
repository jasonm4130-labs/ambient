import { execFile } from "node:child_process";
import { stat } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const cache = new Map<string, Date | undefined>();

function within(root: string, candidate: string): boolean {
  const path = relative(root, candidate);
  return path === "" || (!path.startsWith(`..${sep}`) && path !== "..");
}

async function repositoryRoot(): Promise<string | undefined> {
  try {
    const { stdout } = await execFileAsync("git", ["rev-parse", "--show-toplevel"], {
      cwd: process.cwd(),
      windowsHide: true,
    });
    return stdout.trim() || undefined;
  } catch {
    return undefined;
  }
}

function sourcePath(root: string, filePath: string): string | undefined {
  const siteRoot = resolve(process.cwd());
  const normalized = filePath.replaceAll("\\", "/");
  const candidate = isAbsolute(filePath)
    ? resolve(filePath)
    : resolve(normalized.startsWith("docs/") ? root : siteRoot, filePath);

  return within(resolve(root, "docs"), candidate) ? candidate : undefined;
}

/** Resolve the newest author timestamp for a source entry without shell parsing. */
export async function getLastUpdated(filePath?: string): Promise<Date | undefined> {
  if (!filePath) return undefined;

  const root = await repositoryRoot();
  if (!root) return undefined;

  const source = sourcePath(root, filePath);
  if (!source) return undefined;

  const relativePath = relative(root, source);
  const cached = cache.get(relativePath);
  if (cached !== undefined || cache.has(relativePath)) return cached;

  try {
    if (!(await stat(source)).isFile()) return undefined;
    const { stdout } = await execFileAsync(
      "git",
      ["log", "-1", "--format=%aI", "--", relativePath],
      { cwd: root, windowsHide: true },
    );
    const value = stdout.trim();
    const date = value ? new Date(value) : undefined;
    const result = date !== undefined && !Number.isNaN(date.valueOf()) ? date : undefined;
    cache.set(relativePath, result);
    return result;
  } catch {
    cache.set(relativePath, undefined);
    return undefined;
  }
}
