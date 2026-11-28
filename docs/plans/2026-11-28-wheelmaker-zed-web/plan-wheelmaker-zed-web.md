# WheelMaker Zed Web Implementation Plan

> **For agentic workers:** REQUIRED SKILL: Use implement to execute this plan task-by-task. Respect the handed-off git_state: inherit prepared, otherwise prepare; use git-workflow for checkpoint/finalize. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open and edit a registered Windows Hub project through the WheelMaker Zed HTTPS site.

**Scope Source:** `docs/scope/2026-11-27-wheelmaker-zed-web.md`

**Architecture:** Gateway installs the released browser package and pins a version to each editor session. Registry exchanges its browser identity for a Zed site session and authorizes projects from its existing directory. The Hub runs the matching executable through the authenticated shared tunnel.

**Tech Stack:** Go, Caddy, yamux, Rust, WebGPU, and GitHub Actions.

**Verification:** Exercise package activation, Registry grants, WS relay, Zed file constraints, and a browser edit and Go language-server session.

---

- [ ] Add a shared release reader in `server/shared/zedrelease`. Verify the stable pointer, manifest, packages, and extraction paths. Test reuse and failed activation against an HTTP fixture.
- [ ] Wire `registry.zed.publicUrl` through config and Gateway Caddy compilation. Keep the previous usable release after a failed refresh. Serve compressed assets at versioned paths.
- [ ] Add Zed site exchange and project endpoints in `server/registry`. Reuse the Registry web session and project scope. Exercise authorized opens, one-time exchange, logout, and revocation.
- [ ] Bind grants to session, project, Hub, and release identity. Relay matched workspace sessions through the existing authenticated Hub tunnel. Close relays after revocation, disconnect, and expiry.
- [ ] Install the pinned program and start a separate process per editor session in `server/hub`. Verify process cleanup and reuse of the installed binary.
- [ ] Apply the project filesystem boundary in `crates/remote_server`. Test reads and writes in the project and read-only definition jumps into dependencies.
- [ ] Add the WheelMaker shell, editor session URLs, and safe switching to `script/windows-local-web` and `crates/zed_web_workspace`. Verify readiness, save confirmation, and failed switches.
- [ ] Build the matched release from Zed. Measure the optimized cold transfer and check Go completion, hover, inlay hints and definition, CJK rendering, and TSX highlighting.
- [ ] Run the HTTPS integration with two Windows Hubs. Check scope, simultaneous sessions, failure handling, and cleanup. Document the observed workflow in the WheelMaker Gateway and release pages.
