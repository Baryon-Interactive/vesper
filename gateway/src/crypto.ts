import { digest, encoder, hex, unb64, unhex } from "./common";

export const canonical = (
  method: string,
  path: string,
  timestamp: string,
  bodyHash: string,
) => `${method.toUpperCase()}\n${path}\n${timestamp}\n${bodyHash}`;

export async function importEd25519(hexKey: string, usage: KeyUsage[]) {
  return crypto.subtle.importKey(
    "raw",
    unhex(hexKey),
    { name: "Ed25519", namedCurve: "Ed25519" },
    false,
    usage,
  );
}

export async function masterCrypto(
  secret: string,
  mode: "encrypt" | "decrypt",
  value: string,
) {
  const keyBytes = await digest(secret);
  const key = await crypto.subtle.importKey(
    "raw",
    keyBytes.buffer as ArrayBuffer,
    "AES-GCM",
    false,
    [mode],
  );
  if (mode === "encrypt") {
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ciphertext = new Uint8Array(
      await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, unhex(value)),
    );
    return hex(iv) + hex(ciphertext);
  }
  const raw = unhex(value);
  const iv = raw.slice(0, 12);
  const plaintext = new Uint8Array(
    await crypto.subtle.decrypt({ name: "AES-GCM", iv }, key, raw.slice(12)),
  );
  return hex(plaintext);
}

export async function verifyThreshold(
  payload: unknown,
  signatures: unknown,
  keys: Record<string, string>,
  threshold: number,
): Promise<boolean> {
  if (!Array.isArray(signatures)) return false;
  const unsigned =
    payload && typeof payload === "object"
      ? Object.fromEntries(
          Object.entries(payload as Record<string, unknown>).filter(
            ([key]) => key !== "signatures",
          ),
        )
      : payload;
  const data = encoder.encode(JSON.stringify(unsigned));
  let valid = 0;
  for (const item of signatures) {
    const signature = item as Record<string, unknown>;
    const keyHex = keys[String(signature.keyid)];
    if (!keyHex || typeof signature.sig !== "string") continue;
    try {
      const key = await importEd25519(keyHex, ["verify"]);
      if (
        await crypto.subtle.verify(
          "Ed25519",
          key,
          unb64(String(signature.sig)),
          data,
        )
      ) {
        valid += 1;
      }
    } catch {
      // Ignore malformed signatures and continue counting valid signatures.
    }
  }
  return valid >= threshold;
}
