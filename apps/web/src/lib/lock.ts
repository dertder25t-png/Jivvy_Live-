/**
 * Volunteer mode: this browser locked to the operator view until someone enters the PIN it
 * was locked with. The lock lives in this browser only (no account or cloud needed): just a
 * salted hash of the PIN. Clearing the browser's data does remove it, but also removes this
 * device's key, so the church computer has to approve the device again before it can do anything.
 */
/** Other tabs watch this key (`storage` events), so locking one tab locks them all. */
export const LOCK_KEY = "jivvy.lock";
const KEY = LOCK_KEY;

interface Lock {
  salt: string;
  hash: string;
}

/**
 * Where the browser blocks storage, the lock holds for this visit. Used only then: with
 * storage working, what is stored is the truth (another tab may have unlocked).
 */
let inMemory: Lock | null = null;

export const validPin = (pin: string): boolean => /^\d{4,8}$/.test(pin);

export function isLocked(): boolean {
  return read() !== null;
}

function read(): Lock | null {
  if (inMemory) return inMemory; // only set when storage refused the lock
  try {
    const raw = localStorage.getItem(KEY);
    return raw ? (JSON.parse(raw) as Lock) : null;
  } catch {
    return null; // blocked storage and no lock this visit
  }
}

async function hash(salt: string, pin: string): Promise<string> {
  const bytes = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(`${salt}:${pin}`));
  return [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Locks this browser to the operator view. */
export async function lock(pin: string): Promise<void> {
  if (!validPin(pin)) throw new Error("The PIN is 4 to 8 digits.");
  const salt = [...crypto.getRandomValues(new Uint8Array(16))].map((b) => b.toString(16).padStart(2, "0")).join("");
  const locked: Lock = { salt, hash: await hash(salt, pin) };
  try {
    localStorage.setItem(KEY, JSON.stringify(locked));
    inMemory = null;
  } catch {
    inMemory = locked; // storage refused: locked for this visit only
  }
}

/** Wrong PINs in a row before a short wait, and how long the wait is. */
const TRIES = 5;
const WAIT_MS = 30_000;
let wrong = 0;
let waitUntil = 0;

/** Unlocks with the right PIN; otherwise says why not. */
export async function unlock(pin: string, now = Date.now()): Promise<{ ok: true } | { ok: false; message: string }> {
  const current = read();
  if (!current) return { ok: true };
  if (now < waitUntil) return { ok: false, message: `Too many tries. Wait ${Math.ceil((waitUntil - now) / 1000)} s.` };
  if ((await hash(current.salt, pin)) !== current.hash) {
    wrong += 1;
    if (wrong >= TRIES) {
      wrong = 0;
      waitUntil = now + WAIT_MS;
    }
    return { ok: false, message: "That PIN isn't right." };
  }
  wrong = 0;
  inMemory = null;
  try {
    localStorage.removeItem(KEY);
  } catch {
    // nothing stored
  }
  return { ok: true };
}
