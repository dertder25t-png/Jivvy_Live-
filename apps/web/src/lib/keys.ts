/**
 * This browser's device key (ECDSA P-256). The private key is created non-extractable and
 * kept in IndexedDB, so page scripts can sign with it but nothing can read it out.
 */
const DB = "jivvy-live";
const STORE = "keys";
const KEY = "device";

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    // No version: whatever version is there opens, so a database a newer version of this page
    // has upgraded still gives back the same key (going back a version never unpairs).
    const req = indexedDB.open(DB);
    req.onupgradeneeded = () => {
      if (!req.result.objectStoreNames.contains(STORE)) req.result.createObjectStore(STORE);
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function run<T>(db: IDBDatabase, mode: IDBTransactionMode, op: (s: IDBObjectStore) => IDBRequest): Promise<T> {
  return new Promise((resolve, reject) => {
    const req = op(db.transaction(STORE, mode).objectStore(STORE));
    req.onsuccess = () => resolve(req.result as T);
    req.onerror = () => reject(req.error);
  });
}

const generate = () =>
  crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign", "verify"]) as Promise<CryptoKeyPair>;

let cached: Promise<CryptoKeyPair> | null = null;

/** The saved key pair, or a new one saved for next time. */
export function deviceKey(): Promise<CryptoKeyPair> {
  cached ??= (async () => {
    try {
      const db = await open();
      const saved = await run<CryptoKeyPair | undefined>(db, "readonly", (s) => s.get(KEY));
      if (saved) return saved;
      const pair = await generate();
      try {
        await run(db, "readwrite", (s) => s.put(pair, KEY));
      } catch {
        // Can't keep it (storage refuses keys): this one works for this visit
      }
      return pair;
    } catch {
      // No usable IndexedDB (some private windows and privacy settings): paired for this visit only
      return generate();
    }
  })();
  return cached;
}

export function base64url(bytes: ArrayBuffer): string {
  let s = "";
  for (const b of new Uint8Array(bytes)) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** Raw uncompressed public key (65 bytes), base64url: what pair.request and auth.hello take. */
export async function publicKey(pair: CryptoKeyPair): Promise<string> {
  return base64url(await crypto.subtle.exportKey("raw", pair.publicKey));
}

/** ECDSA P-256 / SHA-256 over the UTF-8 message; WebCrypto gives r||s, as auth.prove wants. */
export async function sign(pair: CryptoKeyPair, message: string): Promise<string> {
  const data = new TextEncoder().encode(message);
  return base64url(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, pair.privateKey, data));
}
