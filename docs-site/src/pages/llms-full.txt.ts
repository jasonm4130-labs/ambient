// Full-corpus markdown for AI agents — every published page in one
// document. Scope and collation live in the framework helper; reshape or
// delete this route to change the site's corpus policy.
import { renderCorpusMarkdown } from "@cloudflare/nimbus-docs";

import { config } from "virtual:nimbus/config";
import { withBase } from "../lib/urls";

export const prerender = true;

export async function GET() {
  const origin = new URL(config.site).origin;
  const base = withBase("/");
  const corpus = (await renderCorpusMarkdown()).replaceAll(`${origin}/`, (match, offset, text) =>
    text.slice(offset + origin.length).startsWith(base) ? match : `${origin}${base}`,
  );
  return new Response(corpus, {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}
