/**
 * This browser's own storage (IndexedDB): the device key and the images it has added. Kept in
 * the browser until the local database item puts the library on the church computer.
 */
const DB = "jivvy-live";
/** 1: keys. 2: media (image blobs by SHA-256). */
const VERSION = 2;
export type Store = "keys" | "media";

let opened: Promise<IDBDatabase> | null = null;

export function open(): Promise<IDBDatabase> {
  opened ??= openAt(VERSION)
    .catch((e: unknown) => {
      // A newer version of this page has upgraded it: open it as it is (its stores are a
      // superset of ours), so going back a version never loses the key or the pictures.
      if (e instanceof DOMException && e.name === "VersionError") return openAt(undefined);
      throw e;
    })
    .catch((e: unknown) => {
      opened = null; // try again next time
      throw e;
    });
  return opened;
}

function openAt(version: number | undefined): Promise<IDBDatabase> {
  return new Promise<IDBDatabase>((resolve, reject) => {
    const req = version === undefined ? indexedDB.open(DB) : indexedDB.open(DB, version);
    req.onupgradeneeded = () => {
      for (const name of ["keys", "media"] satisfies Store[])
        if (!req.result.objectStoreNames.contains(name)) req.result.createObjectStore(name);
    };
    req.onsuccess = () => {
      // A newer version opened in another tab can upgrade instead of waiting on this one.
      req.result.onversionchange = () => req.result.close();
      resolve(req.result);
    };
    req.onerror = () => reject(req.error);
    // An older version of this page in another tab holds the database open.
    req.onblocked = () =>
      reject(new Error("Jivvy Live is open in another tab with an older version. Close that tab and reload this one."));
  });
}

/**
 * One request in its own transaction. Resolves only once the transaction has committed, so a
 * write that is later rolled back (disk full, quota) is reported, never taken as saved.
 */
export function run<T>(db: IDBDatabase, store: Store, mode: IDBTransactionMode, op: (s: IDBObjectStore) => IDBRequest): Promise<T> {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(store, mode);
    const req = op(tx.objectStore(store));
    tx.oncomplete = () => resolve(req.result as T);
    tx.onabort = () => reject(tx.error ?? req.error ?? new Error("This browser couldn't save it."));
    tx.onerror = () => reject(tx.error ?? req.error);
  });
}
