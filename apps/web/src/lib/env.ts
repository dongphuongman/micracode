/**
 * Client-safe environment accessor.
 *
 * Only `NEXT_PUBLIC_*` variables are exposed to the browser bundle.
 * Local-filesystem build: no auth, no DB credentials — the backend
 * URL is the only thing the client needs.
 */

export const env = {
  API_BASE_URL:
    process.env.NEXT_PUBLIC_API_BASE_URL ?? "",
} as const;

export type Env = typeof env;
