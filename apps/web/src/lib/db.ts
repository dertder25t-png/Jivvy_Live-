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
  opened ??= new Promise((resolve, reject) => {
    const req = indexedDB.open(DB, VERSION);
    req.onupgradeneeded = () => {
      for (const name of ["keys", "media"] satisfies Store[])
        if (!req.result.objectStoreNames.contains(name)) req.result.createObjectStore(name);
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
  return opened;
}

export function run<T>(db: IDBDatabase, store: Store, mode: IDBTransactionMode, op: (s: IDBObjectStore) => IDBRequest): Promise<T> {
  return new Promise((resolve, reject) => {
    const req = op(db.transaction(store, mode).objectStore(store));
    req.onsuccess = () => resolve(req.result as T);
    req.onerror = () => reject(req.error);
  });
}
