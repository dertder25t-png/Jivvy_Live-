export const CONTACT_EMAIL = "jivvysystems@gmail.com";

/**
 * Returns a safe Google Calendar booking-page link, or null when none is configured.
 * Only https appointment-schedule links on Google's hosts are accepted so a typo in
 * the build env can't send visitors somewhere unexpected.
 */
export function bookingUrl(raw: string | undefined): string | null {
  if (!raw?.trim()) return null;
  try {
    const u = new URL(raw.trim());
    if (u.protocol !== "https:" || u.username || u.password) return null;
    const host = u.hostname.toLowerCase();
    const shortLink = host === "calendar.app.google" && u.pathname.length > 1;
    const fullLink = host === "calendar.google.com" && u.pathname.startsWith("/calendar/appointments/");
    return shortLink || fullLink ? u.toString() : null;
  } catch {
    return null;
  }
}
