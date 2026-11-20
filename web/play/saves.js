// Unity WebGL saves for the move from one shared play origin to per-game
// origins. Classic script, loaded by the launch page and the save bridge.
//
// Unity keeps a WebGL game's persistent files (PlayerPrefs and
// Application.persistentDataPath) in the IndexedDB database "/idbfs", object
// store "FILE_DATA", under the key folder "/idbfs/<md5 of the game URL's
// directory>". The directory includes the origin, so the same game on a new
// origin looks in a new folder. Records keep their values as Unity wrote them
// ({timestamp, mode, contents}); only the folder in their keys changes.
"use strict";

self.VesperSaves = (() => {
  const DB = "/idbfs";
  const STORE = "FILE_DATA";
  const VERSION = 21; // Emscripten IDBFS.DB_VERSION, as Unity creates it

  /** MD5 of a string's UTF-8 bytes, as lowercase hex. */
  function md5(text) {
    const bytes = new TextEncoder().encode(text);
    const words = new Uint32Array((((bytes.length + 8) >>> 6) + 1) * 16);
    for (let i = 0; i < bytes.length; i++) words[i >> 2] |= bytes[i] << ((i % 4) * 8);
    words[bytes.length >> 2] |= 0x80 << ((bytes.length % 4) * 8);
    words[words.length - 2] = bytes.length * 8;
    words[words.length - 1] = Math.floor(bytes.length / 0x20000000);
    const S = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    const K = Array.from({ length: 64 }, (_, i) => Math.floor(Math.abs(Math.sin(i + 1)) * 2 ** 32) >>> 0);
    let a0 = 0x67452301;
    let b0 = 0xefcdab89;
    let c0 = 0x98badcfe;
    let d0 = 0x10325476;
    for (let block = 0; block < words.length; block += 16) {
      let [a, b, c, d] = [a0, b0, c0, d0];
      for (let i = 0; i < 64; i++) {
        let f;
        let g;
        if (i < 16) [f, g] = [(b & c) | (~b & d), i];
        else if (i < 32) [f, g] = [(d & b) | (~d & c), (5 * i + 1) % 16];
        else if (i < 48) [f, g] = [b ^ c ^ d, (3 * i + 5) % 16];
        else [f, g] = [c ^ (b | ~d), (7 * i) % 16];
        const sum = (a + f + K[i] + words[block + g]) >>> 0;
        const s = S[(i >> 4) * 4 + (i % 4)];
        [a, d, c] = [d, c, b];
        b = (b + ((sum << s) | (sum >>> (32 - s)))) >>> 0;
      }
      a0 = (a0 + a) >>> 0;
      b0 = (b0 + b) >>> 0;
      c0 = (c0 + c) >>> 0;
      d0 = (d0 + d) >>> 0;
    }
    return [a0, b0, c0, d0].map((v) => [0, 8, 16, 24].map((s) => ((v >>> s) & 0xff).toString(16).padStart(2, "0")).join("")).join("");
  }

  /**
   * Unity's save folder for a game whose entry page is `entry` (for example
   * "index.html" or "Build/index.html") under `<origin>/run/<appId>/`.
   */
  function unityFolder(origin, appId, entry) {
    const dir = entry.split("/").slice(0, -1).map(encodeURIComponent).join("/");
    return `${DB}/${md5(`${origin}/run/${encodeURIComponent(appId)}${dir ? `/${dir}` : ""}`)}`;
  }

  const request = (req) =>
    new Promise((resolve, reject) => {
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });

  async function exists() {
    if (!indexedDB.databases) return true; // cannot tell; opening may create an empty one
    return (await indexedDB.databases()).some((d) => d.name === DB);
  }

  /** Open "/idbfs"; with `create`, make it as Unity would. Null when there is none. */
  async function open(create) {
    const present = await exists();
    if (!create && !present) return null;
    const req = present ? indexedDB.open(DB) : indexedDB.open(DB, VERSION);
    req.onupgradeneeded = () => {
      const store = req.result.objectStoreNames.contains(STORE) ? req.transaction.objectStore(STORE) : req.result.createObjectStore(STORE);
      if (!store.indexNames.contains("timestamp")) store.createIndex("timestamp", "timestamp", { unique: false });
    };
    const db = await request(req);
    if (db.objectStoreNames.contains(STORE)) return db;
    db.close();
    return null;
  }

  // Keys from the folder itself up to everything below it.
  const span = (folder) => IDBKeyRange.bound(folder, `${folder}/\uffff`);
  const inFolder = (key, folder) => typeof key === "string" && (key === folder || key.startsWith(`${folder}/`));

  /** Records under a save folder, keyed by their path inside it ("" is the folder). */
  async function read(folder) {
    const db = await open(false);
    if (!db) return [];
    try {
      const store = db.transaction(STORE, "readonly").objectStore(STORE);
      const [keys, values] = await Promise.all([request(store.getAllKeys(span(folder))), request(store.getAll(span(folder)))]);
      return keys.flatMap((key, i) => (inFolder(key, folder) ? [{ path: key.slice(folder.length), value: values[i] }] : []));
    } finally {
      db.close();
    }
  }

  /**
   * Write records into a save folder unless it already holds any: a save the
   * game made on its new origin is never replaced. Returns the number written.
   */
  async function write(folder, records) {
    const valid = records.filter((r) => r && typeof r.path === "string" && (r.path === "" || r.path.startsWith("/")) && !r.path.includes("/../"));
    if (!valid.length) return 0;
    const db = await open(true);
    if (!db) return 0;
    try {
      const tx = db.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      const done = new Promise((resolve, reject) => {
        tx.oncomplete = resolve;
        tx.onerror = () => reject(tx.error);
        tx.onabort = () => reject(tx.error);
      });
      const present = await request(store.getAllKeys(span(folder)));
      if (present.some((key) => inFolder(key, folder))) {
        tx.abort();
        await done.catch(() => {});
        return 0;
      }
      for (const r of valid) store.put(r.value, folder + r.path);
      await done;
      return valid.length;
    } finally {
      db.close();
    }
  }

  return { md5, unityFolder, read, write };
})();
