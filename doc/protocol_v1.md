# WaterDrop Protocol v1

Status: MVP
Scope: Linux receiver/sender (headless first), Android receiver/sender, LAN + Tailscale
Transport: Custom framed protocol over TCP

## Goals

- Simple, implementable MVP protocol for:
  - Device pairing (code or NAS password)
  - Authenticated file transfer
  - Receiver-controlled acceptance (`auto_accept` or prompt)
  - Multiple simultaneous transfers (handled via concurrent TCP connections)
- Works on LAN and Tailscale

---

## Terminology

- **Sender**: device initiating a file upload.
- **Receiver**: device that accepts and stores a file in a configured drop directory.
- **Paired device**: device whose identity public key is stored in the receiver's paired database.
- **Pairing mode**: time-limited receiver state that allows adding new paired devices.
- **Transfer**: one offered file, identified by a `transfer_id`.

---

## Port and addressing

- Default port: `4242` (configurable)
- Receiver binds to `0.0.0.0:<port>` (or a specific interface).
- Sender connects to `receiver_host:port` (LAN IP or Tailscale IP/hostname).

---

## Concurrency model

- **One transfer per TCP connection**.
- Receiver MAY accept many inbound connections at the same time and process them concurrently.
- Each connection corresponds to one `transfer_id`.

## Transfer identifier (`transfer_id`)

- `transfer_id` MUST be present in `TRANSFER_OFFER`, `TRANSFER_DECISION`, and `TRANSFER_DONE`.
- Sender MUST generate a unique `transfer_id` per offered file.

---

## Frame format

All control messages are sent in frames:

```
+---------+---------+---------+---------+--------------+------------------+
| magic   | version | type    | flags   | length (u32) | payload (bytes)  |
| 5 bytes | 1 byte  | 1 byte  | 2 bytes | 4 bytes      | length bytes     |
+---------+---------+---------+---------+--------------+------------------+
```

- `magic`: ASCII `WDROP` (5 bytes)
- `version`: `0x01`
- `type`: message type (u8), see `Message types`
- `flags`: reserved, MUST be `0x0000` in v1
- `length`: big-endian u32 payload length
- `payload`: UTF-8 JSON

---

## Message types

### Type codes (v1)

| Type (u8) | Name                  | Direction          |
|----------:|-----------------------|--------------------|
| 0x01      | HELLO                 | Sender -> Receiver |
| 0x02      | HELLO_ACK             | Receiver -> Sender |
| 0x10      | PAIR_WITH_CODE        | Sender -> Receiver |
| 0x11      | PAIR_WITH_PASSWORD    | Sender -> Receiver |
| 0x12      | PAIR_RESULT           | Receiver -> Sender |
| 0x20      | TRANSFER_OFFER        | Sender -> Receiver |
| 0x21      | TRANSFER_DECISION     | Receiver -> Sender |
| 0x30      | TRANSFER_DONE         | Receiver -> Sender |
| 0x7F      | ERROR                 | Either direction   |

---

## Identity and authentication

Each device has a long-term identity keypair:
- Algorithm: Ed25519
- `device_id`: computed as a stable fingerprint of the public key (e.g. SHA-256(pubkey) hex truncated).

Receiver stores paired devices:
- `device_id`
- `device_name`
- `device_pubkey`

### MVP authentication handshake

The goal is: receiver only accepts transfers from paired devices, or allows pairing only in pairing mode.

#### HELLO

Sender begins every connection with `HELLO`.

Payload:
```json
{
  "device_id": "string",
  "device_name": "string",
  "timestamp_unix_ms": 0,
  "nonce_b64": "string",
  "signature_b64": "string"
}
```

Signature:
- `signature = Sign(sender_identity_sk, nonce || timestamp_unix_ms || device_id)`
- Receiver verifies signature using stored public key for `device_id`.

#### HELLO_ACK

Receiver response:
```json
{
  "ok": true,
  "paired": true
}
```

Rules:
- If `device_id` is already paired and signature validates: `ok=true`
- If not paired:
  - `ok=false` unless receiver is in pairing mode
  - If receiver is in pairing mode: `ok=true` BUT receiver MUST restrict next allowed messages to pairing messages until pairing succeeds.

---

## Pairing

Pairing may be enabled by:
- Linux interactive user (CLI/UI)
- NAS admin (CLI/env) with TTL

### PAIR_WITH_CODE (0x10)

Used for interactive pairing where receiver displays a short code.

Payload:
```json
{
  "code": "string",
  "new_device_id": "string",
  "new_device_name": "string",
  "new_device_pubkey_b64": "string"
}
```

Receiver validates:
- Pairing mode is enabled and not expired
- `code` matches receiver's displayed code

If valid:
- Receiver stores the device as paired.

### PAIR_WITH_PASSWORD (0x11)

Used for NAS/headless pairing.

Payload:
```json
{
  "password": "string",
  "new_device_id": "string",
  "new_device_name": "string",
  "new_device_pubkey_b64": "string"
}
```

Receiver validates:
- Pairing mode enabled and not expired
- Password matches configured password

If valid:
- Receiver stores the device as paired.

### PAIR_RESULT (0x12)

Receiver response to either pairing request:
```json
{
  "ok": true
}
```

After `ok=true`, receiver SHOULD allow transfers from this device for the remainder of the connection.

---

## File transfer

### TRANSFER_OFFER (0x20)

Sender offers a file.

Payload:
```json
{
  "transfer_id": "string",
  "filename": "string",
  "size_bytes": 0,
  "sha256_hex": "string"
}
```

Receiver behavior:
- If sender is not paired: respond `ERROR` (or deny).
- If receiver `auto_accept=true`: accept immediately.
- If receiver requires user approval: receiver MAY delay responding until user decides, up to a timeout.

### TRANSFER_DECISION (0x21)

Receiver decision.

Payload:
```json
{
  "transfer_id": "string",
  "accept": true
}
```

If `accept=false`, sender MUST stop and may close the connection.

### File bytes stream

If accepted:
- Sender sends exactly `size_bytes` raw bytes immediately after the `TRANSFER_DECISION(accept=true)` frame.
- No additional framing is used for the raw bytes stream in v1.

Receiver MUST:
- Read exactly `size_bytes` bytes
- Write to a temp file under the drop directory (or a temp subdir)
- On success, atomically rename to final destination filename
- Handle collisions by renaming (e.g. `file (1).ext`)

### TRANSFER_DONE (0x30)

Receiver sends result after storing the file.

Payload:
```json
{
  "transfer_id": "string",
  "ok": true,
  "stored_filename": "string (optional)",
  "reason": "string (optional)"
}
```

- `stored_filename` is the final name after sanitization/collision handling.

---

## ERROR (0x7F)

Either side can send an error.

Payload:
```json
{
  "code": "string",
  "message": "string"
}
```

Error codes:
- `bad_frame`
- `unsupported_version`
- `not_paired`
- `pairing_disabled`
- `pairing_failed`
- `auth_failed`
- `transfer_denied`
- `invalid_filename`
- `io_error`
