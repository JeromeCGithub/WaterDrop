# WaterDrop Architecture (V1)

## Repo structure (Cargo workspace)
- `waterdrop-core` (lib)
- `waterdrop-engine` (lib)
- `waterdrop-cli` (bin, headless/NAS)
- `waterdrop-app` (bin, UI with Dioxus)

Dependency direction:
- `waterdrop-cli` -> `waterdrop-engine` -> `waterdrop-core`
- `waterdrop-app` -> `waterdrop-engine` -> `waterdrop-core`

## waterdrop-core (library)
Purpose: shared building blocks.
- Protocol:
  - frame format (magic/version/type/len)
  - message structs/enums (pairing + transfers)
  - payload encoding (JSON or CBOR; decide once)
- Identity/auth primitives:
  - device keypair
  - device_id (pubkey fingerprint)
  - signature helpers
  - (optional later) TLS cert helpers + fingerprint
- Persistence models:
  - paired devices records
  - config model
- Filesystem helpers:
  - sanitize filename
  - temp write + atomic rename
  - collision rename strategy

## waterdrop-engine (library)
Purpose: runtime logic embedded into CLI and UI.
- TCP listener + per-connection session tasks
- Auth:
  - verify HELLO signature for paired devices
  - allow pairing only in pairing mode (TTL)
- Pairing subsystem:
  - code pairing
  - NAS password pairing (hash compare + rate limit)
  - persist new paired devices
- Transfer manager:
  - incoming offers -> accept/deny -> receive bytes -> finalize file
  - concurrent transfers (one per connection)
  - limits (max concurrent total/per-device)
- Event bus:
  - emits events for UI/CLI (offer, progress, done, errors)

## waterdrop-cli (binary)
Purpose: headless receiver and NAS operation.
- Starts engine in headless mode.
- Exposes commands to:
  - run receiver (`headless`)
  - enable pairing (prints code/TTL)
  - list/remove paired devices
- Logs events to stdout (Docker-friendly).

## waterdrop-app (binary, Dioxus UI)
Purpose: Linux desktop UX.
- Starts engine in-process.
- Subscribes to engine events.
- UI features:
  - show status + drop folder
  - enable pairing (display code)
  - prompt accept/deny when `auto_accept=false`
  - show transfer progress/history
