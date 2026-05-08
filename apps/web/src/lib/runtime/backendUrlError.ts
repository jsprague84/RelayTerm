/**
 * UI-side mapping from `BackendUrlError` (design § 10) to a static
 * sentence the picker can render. Each branch returns a redacted
 * string — the input URL is never echoed; the only signal is the
 * typed reason. Mirrors the redaction posture of `describeAuthError`
 * / `describeLoadError` in `apps/web/src/lib/api/`.
 *
 * Pure function. No DOM, no logging. Unit-tested separately so the
 * picker component stays mostly markup.
 */

import type { BackendUrlError } from "./backendConfig.js";

export function describeBackendUrlError(reason: BackendUrlError): string {
  switch (reason) {
    case "url_empty":
      return "Enter the URL of your RelayTerm server.";
    case "url_too_long":
      return "URL is too long to be a valid origin.";
    case "url_parse_failed":
      return "That doesn't look like a valid URL. Try https://relayterm.example.com.";
    case "url_credentials_forbidden":
      return "Remove the username/password from the URL — credentials are never part of the server address.";
    case "url_scheme_forbidden":
      return "Use https:// (or http:// for localhost only).";
    case "url_http_non_localhost":
      return "Use https:// for any host other than localhost.";
    case "url_path_forbidden":
      return "Drop any path — only the bare origin (https://host[:port]) is accepted.";
    case "url_search_forbidden":
      return "Drop any query string — only the bare origin is accepted.";
    case "url_hash_forbidden":
      return "Drop any '#' fragment — only the bare origin is accepted.";
  }
}
