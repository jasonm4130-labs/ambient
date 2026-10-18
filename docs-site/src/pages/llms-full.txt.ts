// Full-corpus markdown for AI agents — every published page in one
// document. Scope, collation and the deployment base live in the framework
// route; reshape or delete this route to change the site's corpus policy.
import { llmsFullRoute } from "@cloudflare/nimbus-docs/agent-endpoints";

export const prerender = true;
export const { GET } = llmsFullRoute();
