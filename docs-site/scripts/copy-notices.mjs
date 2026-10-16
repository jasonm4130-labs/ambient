import { cp, copyFile } from "node:fs/promises";

// Keep the notices with the static artifact, including offline previews.
const root = new URL("../../", import.meta.url);
const dist = new URL("../dist/", import.meta.url);
for (const file of ["LICENSE", "THIRD_PARTY_NOTICES.md"]) {
  await copyFile(new URL(file, root), new URL(file, dist));
}
await cp(new URL("licenses/", root), new URL("licenses/", dist), { recursive: true });
