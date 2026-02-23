# WaterDrop

WaterDrop is a LAN/Tailscale file drop tool (AirDrop-like) for **Linux + Android + Windows**.

## Goals
- Send files quickly to a trusted device on:
  - Local network (LAN)
  - Tailscale network
- **Device pairing** is the security model.
- Receiver saves to a configured **drop folder** (NAS-friendly).
- Can run:
  - as a **Linux desktop app** (UI)
  - as a **headless** process in **Docker/unRAID** (NAS)

## Pairing
Two pairing methods:
- **Code pairing (interactive)**: receiver shows a short code; sender enters it.
- **NAS password pairing (headless)**: sender provides a NAS pairing password (stored hashed on NAS).
- Pairing should be enabled only temporarily (TTL) to reduce risk.

After pairing, devices are remembered by storing the peer identity public key.

## Receiving policy
- Interactive devices: can prompt user to accept/deny each incoming transfer.
- NAS/headless: configurable `auto_accept=true/false`.
- All accepted files are stored inside the configured drop folder.

## Transport
- Custom protocol over **TCP** (LAN/Tailscale).
- One transfer per TCP connection.
- Multiple simultaneous transfers are supported via multiple concurrent connections.
- TLS over TCP is optional for later; Tailscale provides encryption when used.
