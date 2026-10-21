// Church accounts on live.jivvy.org: email sign-in with a one-time code, kept minimal.
// Nothing here is needed during a service: the church computer checks its license offline.

/** How long a sign-in code works. */
export const CODE_TTL_MS = 10 * 60_000;
/** Wrong tries before a code stops working (ask for a new one). */
export const MAX_CODE_ATTEMPTS = 5;
/** A new code for the same address is sent at most this often. */
export const RESEND_MS = 60_000;
/** How long a sign-in lasts. */
export const SESSION_TTL_MS = 30 * 24 * 60 * 60_000;
export const SESSION_COOKIE = "jl_session";

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]{2,}$/;

/** A trimmed, lowercased email address, or null. */
export function normalEmail(x: unknown): string | null {
  if (typeof x !== "string") return null;
  const email = x.trim().toLowerCase();
  return email.length <= 254 && EMAIL.test(email) ? email : null;
}

/** Six random digits. */
export function newCode(): string {
  const n = crypto.getRandomValues(new Uint32Array(1))[0]! % 1_000_000;
  return n.toString().padStart(6, "0");
}

const b64url = (bytes: Uint8Array) =>
  btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

/** A random token for a session cookie (32 bytes, base64url). */
export const newToken = () => b64url(crypto.getRandomValues(new Uint8Array(32)));

/** A church id the license key accepts (1-64 of A-Z a-z 0-9 _ -). */
export const newChurchId = () => `ch_${b64url(crypto.getRandomValues(new Uint8Array(12)))}`;

/** SHA-256, hex. Codes and session tokens are stored only as this. */
export async function sha256Hex(s: string): Promise<string> {
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s)));
  return [...d].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** What is stored for a code: bound to the address, so a code for one address never works for another. */
export const codeHash = (email: string, code: string) => sha256Hex(`signin\n${email}\n${code}`);

/** The code as typed: digits only, so "123 456" works. */
export const typedCode = (x: unknown) => (typeof x === "string" ? x.replace(/\D/g, "") : "");

export function sessionCookie(token: string, maxAgeMs: number): string {
  return `${SESSION_COOKIE}=${token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=${Math.floor(maxAgeMs / 1000)}`;
}

export function readCookie(header: string | null, name: string): string | null {
  for (const part of (header ?? "").split(";")) {
    const [k, ...v] = part.trim().split("=");
    if (k === name) return v.join("=") || null;
  }
  return null;
}

/** The sign-in email. Plain text; it goes only to the address that asked to sign in. */
export function signInEmail(code: string): { subject: string; text: string } {
  return {
    subject: `Your Jivvy Live sign-in code: ${code}`,
    text: `Your sign-in code is ${code}. It works for 10 minutes.\n\nIf you didn't ask to sign in to Jivvy Live, you can ignore this email.\n`,
  };
}
