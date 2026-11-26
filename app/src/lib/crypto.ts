const HKDF_INFO = new TextEncoder().encode("RemoteConnect-E2E-v1-AES256GCM");

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

export function base64ToBytes(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Ephemeral P-256 ECDH keypair interoperable with `remoteconnect-protocol` Rust crypto.
 */
export class E2eKeyPair {
  private constructor(
    private readonly keyPair: CryptoKeyPair,
    public readonly publicKeyB64: string
  ) {}

  static async generate(): Promise<E2eKeyPair> {
    const keyPair = await crypto.subtle.generateKey(
      { name: "ECDH", namedCurve: "P-256" },
      false,
      ["deriveBits"]
    );
    const rawPub = new Uint8Array(
      await crypto.subtle.exportKey("raw", keyPair.publicKey)
    );
    return new E2eKeyPair(keyPair, bytesToBase64(rawPub));
  }

  async deriveSession(
    peerPublicKeyB64: string,
    sessionId: string
  ): Promise<E2eSession> {
    const peerRaw = base64ToBytes(peerPublicKeyB64);
    const peerPub = await crypto.subtle.importKey(
      "raw",
      peerRaw.buffer as ArrayBuffer,
      { name: "ECDH", namedCurve: "P-256" },
      false,
      []
    );

    const sharedBits = await crypto.subtle.deriveBits(
      { name: "ECDH", public: peerPub },
      this.keyPair.privateKey,
      256
    );

    const hkdfKey = await crypto.subtle.importKey(
      "raw",
      sharedBits,
      "HKDF",
      false,
      ["deriveKey"]
    );

    const aesKey = await crypto.subtle.deriveKey(
      {
        name: "HKDF",
        hash: "SHA-256",
        salt: new TextEncoder().encode(sessionId),
        info: HKDF_INFO,
      },
      hkdfKey,
      { name: "AES-GCM", length: 256 },
      false,
      ["encrypt", "decrypt"]
    );

    return new E2eSession(aesKey);
  }
}

export class E2eSession {
  constructor(private readonly key: CryptoKey) {}

  async encryptBytes(plaintext: Uint8Array): Promise<Uint8Array> {
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ct = new Uint8Array(
      await crypto.subtle.encrypt(
        { name: "AES-GCM", iv },
        this.key,
        plaintext.buffer.slice(
          plaintext.byteOffset,
          plaintext.byteOffset + plaintext.byteLength
        ) as ArrayBuffer
      )
    );
    const out = new Uint8Array(12 + ct.length);
    out.set(iv, 0);
    out.set(ct, 12);
    return out;
  }

  async decryptBytes(data: Uint8Array): Promise<Uint8Array> {
    if (data.length < 28) {
      throw new Error("Ciphertext too short");
    }
    const iv = new Uint8Array(data.subarray(0, 12));
    const ct = data.subarray(12);
    const plain = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv },
      this.key,
      ct.buffer.slice(ct.byteOffset, ct.byteOffset + ct.byteLength) as ArrayBuffer
    );
    return new Uint8Array(plain);
  }

  async encryptJson(obj: unknown): Promise<string> {
    const bytes = new TextEncoder().encode(JSON.stringify(obj));
    const enc = await this.encryptBytes(bytes);
    return bytesToBase64(enc);
  }

  async decryptJson<T = unknown>(payloadB64: string): Promise<T> {
    const raw = base64ToBytes(payloadB64);
    const plain = await this.decryptBytes(raw);
    return JSON.parse(new TextDecoder().decode(plain)) as T;
  }
}
