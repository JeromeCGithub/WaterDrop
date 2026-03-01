# WaterDrop Protocol v1

Status: MVP
Scope: Linux receiver/sender (headless first), Android receiver/sender, LAN + Tailscale
Transport: Custom framed protocol over QUIC/TCP

## Goals

- Simple, implementable MVP protocol for:
  - Authenticated file transfer
  - Receiver-controlled acceptance (`auto_accept` or prompt)
  - Multiple simultaneous transfers (handled via concurrent connections)
- Works on LAN and Tailscale

---

## Terminology

- **Sender**: device initiating a file upload.
- **Receiver**: device that accepts and stores a file in a configured drop directory.
- **Transfer**: one offered file, identified by a `transfer_id`.

---

## Port and addressing

- Default port: `4242` (configurable)
- Receiver binds to `0.0.0.0:<port>` (or a specific interface).
- Sender connects to `receiver_host:port` (LAN IP or Tailscale IP/hostname).

---

## Concurrency model

- **One transfer per connection**.
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
| 0x20      | TRANSFER_OFFER        | Sender -> Receiver |
| 0x21      | TRANSFER_DECISION     | Receiver -> Sender |
| 0x30      | TRANSFER_DONE         | Receiver -> Sender |
| 0x7F      | ERROR                 | Either direction   |

---

## Handshake

### HELLO (0x01)

Sender begins every connection with `HELLO`.

Payload:
```json
{
  "device_name": "string"
}
```

Identifies the sender device by a human-readable name.

### HELLO_ACK (0x02)

Receiver response:
```json
{
  "ok": true,
  "device_name": "string"
}
```

Rules:
- If the receiver is willing to accept connections: `ok=true`
- If the receiver wants to reject: `ok=false`, and the sender MUST close the connection.

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
- `auth_failed`
- `transfer_denied`
- `invalid_filename`
- `io_error`
