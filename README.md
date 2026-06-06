# Zenith

A lightweight, native Minecraft launcher for macOS, written in Rust with [GPUI](https://www.gpui.rs/) (the GPU-accelerated UI framework behind [Zed](https://zed.dev)).

> Status: early and actively built. Vanilla, Fabric, and Quilt launch today; Forge/NeoForge and Modrinth modpacks are on the roadmap.

## Features

- **Real version list** from Mojang's manifest
- **Downloads the game** — client jar, libraries (with OS/arch rules), natives, and assets (parallelized)
- **Launches `java`** with the correct classpath/arguments for both modern and legacy version formats
- **Mod loaders:** Vanilla, Fabric, Quilt (via their meta APIs and `inheritsFrom` merging)
- **Single instance** with start/stop control and a live, copyable **console** of launcher + game output
- **Microsoft sign-in** (device-code OAuth → Xbox Live → XSTS → Minecraft) — fully implemented; needs an Azure app ID approved for the Minecraft API
- Minimal, Zed/Linear-flavored UI (IBM Plex Sans + IBM Plex Mono)

## Requirements

- **Rust nightly** (pinned via `rust-toolchain.toml` — GPUI uses unstable features)
- A **Java** runtime on `PATH` (override with `ZENITH_JAVA`); modern versions need Java 21+
- **macOS** with the Metal toolchain (`xcodebuild -downloadComponent MetalToolchain` if the shader build fails)

## Run

```sh
cargo run
```

Game data lives in `~/Library/Application Support/zenith-launcher/`.

### Microsoft sign-in (optional)

Minecraft auth requires your own Azure app registration (personal-accounts only, public-client flows enabled), and Microsoft must grant it Minecraft API access. Provide the client ID via:

```sh
ZENITH_CLIENT_ID=your-azure-client-id cargo run
```

Without it (or until approved), the launcher runs in **offline** mode.

## Architecture

A Cargo workspace, GPUI isolated to the UI layer:

| Crate            | Responsibility                                                        |
| ---------------- | --------------------------------------------------------------------- |
| `core`           | Domain types (`Loader`, `Account`, `VersionEntry`, `Session`) + log/progress bus. No IO, no GPUI. |
| `net`            | Mojang manifest + Microsoft/Xbox/Minecraft authentication             |
| `store`          | On-disk paths and layout                                              |
| `launch`         | Resolve → download → classpath/natives → spawn/kill the game          |
| `ui`             | Theme/palette, layout helpers, shared widgets                         |
| `app`            | The `zenith-launcher` binary: app state, views, wiring                |

`core`, `net`, `store`, and `launch` are plain Rust (testable without a window); only `ui` and `app` depend on GPUI.

## License

MIT
