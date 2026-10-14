/**
 * This browser's device key (ECDSA P-256). The private key is created non-extractable and
 * kept in IndexedDB, so page scripts can sign with it but nothing can read it out.
 */
import { open, run } from "./db";

const KEY = "device";

const generate = () =>
  crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign", "verify"]) as Promise<CryptoKeyPair>;

let cached: Promise<CryptoKeyPair> | null = null;

/** The saved key pair, or a new one saved for next time. */
export function deviceKey(): Promise<CryptoKeyPair> {
  cached ??= (async () => {
    let db: IDBDatabase;
    try {
      db = await open();
    } catch {
      return generate(); // no IndexedDB (some private windows): paired for this visit only
    }
    const saved = await run<CryptoKeyPair | undefined>(db, "keys", "readonly", (s) => s.get(KEY));
    if (saved) return saved;
    const pair = await generate();
    await run(db, "keys", "readwrite", (s) => s.put(pair, KEY));
    return pair;
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
