# WaterDrop

WaterDrop is a LAN/Tailscale file drop tool (AirDrop-like) for **Linux + Android + Windows**.

## Goals
- Send files quickly to a trusted device on:
  - Local network (LAN)
  - Tailscale network
- **Transfer accept/deny** is the security model — the receiver controls what lands on disk.
- Receiver saves to a configured **drop folder** (NAS-friendly).
- Can run:
  - as a **Linux desktop app** (UI)
  - as a **headless** process in **Docker/unRAID** (NAS)

## Receiving policy
- Interactive devices: can prompt user to accept/deny each incoming transfer.
- NAS/headless: configurable `auto_accept=true/false`.
- All accepted files are stored inside the configured drop folder.

## Transport
- Custom protocol over **TCP** (LAN/Tailscale).
- One transfer per TCP connection.
- Multiple simultaneous transfers are supported via multiple concurrent connections.
- TLS over TCP is optional for later; Tailscale provides encryption when used.