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
  - message structs/enums (transfers)
  - payload encoding (JSON)
- TLS helpers:
  - self-signed certificate generation
- Filesystem helpers:
  - sanitize filename
  - temp write + atomic rename
  - collision rename strategy

## waterdrop-engine (library)
Purpose: runtime logic embedded into CLI and UI.
- QUIC/TCP listener + per-connection session tasks
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
  - connect to a peer and send a file
  - accept/deny incoming transfers
- Logs events to stdout (Docker-friendly).

## waterdrop-app (binary, Dioxus UI)
Purpose: Linux desktop UX.
- Starts engine in-process.
- Subscribes to engine events.
- UI features:
  - show status + drop folder
  - prompt accept/deny when `auto_accept=false`
  - show transfer progress/history