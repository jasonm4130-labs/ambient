import { defineCollection } from "astro:content";
// `z` re-exported from `astro:content` is deprecated; import it from
// `astro/zod` (the pattern nimbus-docs' own schema helpers document).
import { z } from "astro/zod";
import { docsCollection, partialsCollection } from "@cloudflare/nimbus-docs/content";

export const collections = {
  // The markdown does NOT live in this project. `docs/` at the repo root is the
  // source of truth: it keeps the ADRs on the citable path
  // `docs/adr/NNNN-slug.md` and keeps every page readable in GitHub's file view,
  // diagrams included. `docsCollection` builds its loader base as
  // `./src/content/${base}`, so the relative escape below resolves to `../docs`
  // from this project root — i.e. the sibling directory.
  docs: defineCollection(
    docsCollection({
      base: "../../../docs",
      // TEMPLATE.md is the ADR scaffold, not a page — without the negation it
      // fails the schema for having no frontmatter, which is correct but not
      // useful. The cast is because nimbus-docs types `pattern` as `string`
      // while the Astro glob loader underneath it accepts `string | string[]`;
      // the array is what actually runs.
      pattern: ["**/*.md", "!**/TEMPLATE.md"] as unknown as string,
      schemaFields: {
        // Nimbus docs are agent-friendly by default. Set `audience: human`
        // to flag a page that's written primarily for human readers.
        audience: z.literal("human").optional(),
      },
    }),
  ),
  partials: defineCollection(partialsCollection()),
};
