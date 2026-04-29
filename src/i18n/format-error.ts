/**
 * Format any backend / network / runtime error into a localised user message.
 *
 * # Wire formats supported
 *
 * 1. **Structured** — JSON envelope produced by `IpcError`:
 *    `{"code":"error.no_api_key","params":{"provider":"openai"},"detail":"..."}`
 *    Looked up in i18n `errors` namespace via `t(code, params)`.
 *    `errors.<id>` falls back to `errors.fallback` (which echoes the code).
 *
 * 2. **Bare string** — legacy commands still returning `Result<_, String>`.
 *    Returned as-is. As more commands migrate to `IpcError`, this branch
 *    naturally shrinks.
 *
 * 3. **Error / unknown** — `e instanceof Error` falls through to `e.message`.
 *    Anything else is `String(e)`.
 *
 * Always returns a non-empty string suitable for direct UI display.
 *
 * # Why not throw?
 *
 * UI handlers want a string; centralising the "is this JSON?" sniff here
 * stops every component from re-implementing it. Callers do
 * `setMessage(formatBackendError(e))`.
 */
import i18n from "./index";

interface IpcErrorPayload {
  code?: string;
  params?: Record<string, unknown>;
  detail?: string;
}

export function formatBackendError(err: unknown): string {
  // Normalise to a string we can sniff
  let raw: string;
  if (typeof err === "string") {
    raw = err;
  } else if (err instanceof Error) {
    raw = err.message;
  } else if (err == null) {
    return i18n.t("errors:unknown");
  } else {
    raw = String(err);
  }

  // Try to parse as IpcError JSON envelope
  if (raw.length > 1 && raw[0] === "{") {
    try {
      const parsed = JSON.parse(raw) as IpcErrorPayload;
      if (parsed && typeof parsed.code === "string" && parsed.code.length > 0) {
        // i18n key e.g. "errors:no_api_key" — strip "error." prefix from backend
        const key = parsed.code.startsWith("error.")
          ? `errors:${parsed.code.slice("error.".length)}`
          : `errors:${parsed.code}`;
        const params = (parsed.params ?? {}) as Record<string, unknown>;
        // i18next falls back to defaultValue if key missing
        return i18n.t(key, {
          ...params,
          defaultValue: parsed.detail ?? parsed.code,
        }) as string;
      }
    } catch {
      // not JSON, fall through
    }
  }

  return raw;
}

/**
 * Convenience for the common `t("common:errorPrefix", { message: ... })`
 * pattern that prepends "Error: ".
 */
export function formatErrorWithPrefix(err: unknown): string {
  return i18n.t("common:errorPrefix", { message: formatBackendError(err) }) as string;
}
