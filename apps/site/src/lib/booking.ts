export const CONTACT_EMAIL = "jivvysystems@gmail.com";

/**
 * Returns a safe Calendly link, or null when none is configured.
 * Only https links on calendly.com are accepted so a typo in the build env
 * can't send visitors somewhere unexpected.
 */
export function bookingUrl(raw: string | undefined): string | null {
  if (!raw?.trim()) return null;
  try {
    const u = new URL(raw.trim());
    const host = u.hostname.toLowerCase();
    if (u.protocol !== "https:" || (host !== "calendly.com" && !host.endsWith(".calendly.com"))) return null;
    if (u.username || u.password) return null;
    return u.toString();
  } catch {
    return null;
  }
}
