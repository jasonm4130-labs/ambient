// Resolves every relative markdown link under docs/ (and in README.md) against
// the filesystem.
//
// This exists because nothing else checks them. `nimbus-docs lint` is MDX-only
// and takes no path, so it never sees docs/**/*.md; the Astro build resolves
// links at render time and does not fail on a dead one; and README.md is
// outside every content pipeline in the repo. A moved page is otherwise a 404
// nobody notices until a reader hits it.
//
// Links inside backticks are skipped: prose quotes paths that are examples, not
// references.
import { readFileSync } from "node:fs";
import { existsSync } from "node:fs";
import { globSync } from "node:fs";
import { dirname, join, normalize } from "node:path";

const files = [...globSync("docs/**/*.md"), "README.md"];
const LINK = /\]\(((?!https?:|#|mailto:)[^)]*?\.md)(?:#[^)]*)?\)/g;

let broken = 0;
for (const file of files) {
	const source = readFileSync(file, "utf8").replace(/`[^`]*`/g, "");
	for (const [, href] of source.matchAll(LINK)) {
		const target = normalize(join(dirname(file), href));
		if (!existsSync(target)) {
			console.error(`::error file=${file}::broken link -> ${href}`);
			broken++;
		}
	}
}

console.log(`${files.length} file(s) scanned, ${broken} broken link(s)`);
process.exit(broken === 0 ? 0 : 1);
