# Dash USB Web Frontend

React single-page app served by the `dashusb` Rust daemon on the Pi.

## Tech stack

- **React 19** + TypeScript
- **Vite** for build tooling and the dev server
- **TailwindCSS** for styling
- **Lucide React** for icons
- **xterm.js** for the Terminal page

## Development

```bash
npm install
npm run dev
```

The dev server listens on `http://localhost:5173` and proxies `/api/*` and
`/Recordings/*` to `http://localhost:8788`. Point it at a live Pi instead of a
local daemon with `DASHUSB_API=http://dashusb.local npm run dev`.

## Production build

Build through the repo root, not from here. `../build.sh` runs `npm run build`,
wipes `crates/sentryusb/static`, copies `dist/` into it, and pre-compresses the
assets; `rust-embed` then bakes that directory into the binary. A bare
`cargo build` without that step embeds the "frontend not built" placeholder from
`crates/sentryusb/build.rs`.

```bash
cd .. && ./build.sh arm64
```

## Pages

| Page | Description |
|------|-------------|
| **Dashboard** | System status, CPU temperature, WiFi, disk space, snapshot and archive state |
| **Viewer** | Multi-camera clip viewer with synced playback. Camera list and grid come from `GET /api/profile`, so they follow the vehicle profile rather than being hardcoded |
| **Files** | Browse, upload, and delete under `/mutable` and `/mutable/Recordings` |
| **Snapshots** | List, create, and release reflink snapshots |
| **Logs** | Live tail of archiveloop, setup, and diagnostics logs |
| **Notifications** | Notification history and per-channel test sends |
| **Terminal** | Shell session over WebSocket |
| **Support** | Support tickets, raised and read in-app |
| **Settings** | Setup wizard, device/network/notifications/system tabs, health check, OTA update, reboot |

## Structure

```
src/
├── components/
│   ├── layout/        # AppShell, Sidebar, MobileNav, ConnectionBanner
│   ├── settings/      # Settings sections and cards
│   ├── setup/         # SetupWizard + 9 step components
│   ├── ui/            # Shared primitives
│   └── upload/        # Upload widgets
├── pages/             # Top-level routes, plus pages/settings/ tabs
└── lib/               # api.ts, ws.ts (WebSocket hook), units.ts, utils.ts
```
