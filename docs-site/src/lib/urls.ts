/** Prefix a site-relative URL with Astro's configured deployment base. */
export function withBase(path: string): string {
  if (/^(?:[a-z][a-z0-9+.-]*:|\/\/|#)/i.test(path)) return path;
  const base = import.meta.env.BASE_URL.replace(/\/$/, "");
  const normalized = path.startsWith("/") ? path : `/${path}`;
  if (!base || normalized === base || normalized.startsWith(`${base}/`)) return normalized;
  return `${base}${normalized}`;
}

/** Remove Astro's deployment base before passing a route to Nimbus helpers. */
export function withoutBase(path: string): string {
  const base = import.meta.env.BASE_URL.replace(/\/$/, "");
  if (!base) return path;
  if (path === base || path === `${base}/`) return "/";
  return path.startsWith(`${base}/`) ? path.slice(base.length) || "/" : path;
}
