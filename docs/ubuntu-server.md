# Ubuntu server setup

How to move SheetPro development from a desktop (macOS) machine to an Ubuntu server and keep
every gate green there. Target: Ubuntu 24.04 LTS, the same image CI uses
(`.github/workflows/ci.yml`), so anything that passes CI also passes here.

## 1. Server sizing

| Resource | Minimum | Comfortable | Why |
|---|---|---|---|
| CPU | 4 cores | 16+ cores | `cargo xtask check` and the perf suite are compile- and core-bound |
| RAM | 8 GB | 32 GB | release builds of the workspace plus the 1M-row perf suite |
| Disk | 60 GB | 150 GB+ | one `target/` is ~35 GB; each parallel agent adds its own `target/agent-<name>` |
| GPU | none | none | headless rendering uses Mesa's software Vulkan (lavapipe) |

## 2. Before you leave the old machine

Everything tracked is on GitHub; check that nothing is left only locally.

```sh
git status                          # untracked or modified files?
git log origin/main..main --oneline # unpushed commits? push them
git stash list                      # stashes don't travel; apply and commit, or drop
git worktree list                   # agent worktrees under .claude/worktrees/
git branch -vv                      # local-only branches? push or delete them
```

Things git does **not** carry, and what to do with each:

| Item | Where (macOS) | Action |
|---|---|---|
| `target/` | repo | Don't copy; rebuild on the server (it is platform-specific) |
| `corpus/files/` | repo (gitignored) | Re-fetch with `cargo xtask corpus` (sha256-verified) |
| `dist/` | repo (gitignored) | Don't copy; `cargo xtask package` rebuilds it for Linux |
| `.claude/` | repo (untracked) | Local Claude Code settings and agent worktrees; copy `settings*.json` if you have any, remove finished worktrees first (`git worktree remove`) |
| App settings | `~/Library/Application Support/SheetPro/` | Optional: copy `settings.toml` to `~/.config/sheetpro/` |
| Claude Code memory | `~/.claude/projects/<path-slug>/memory/` | Copy; see §6 (the slug changes with the path) |
| Global Claude config | `~/.claude/CLAUDE.md`, `~/.claude/skills/` | Copy if you rely on them |
| Git identity, SSH keys, `gh` login | `~/.gitconfig`, `~/.ssh/` | Recreate on the server (prefer a new SSH key over copying the old one) |

## 3. Install the system packages

```sh
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential curl git ca-certificates pkg-config \
  libxkbcommon-dev libwayland-dev libx11-dev \
  libvulkan1 mesa-vulkan-drivers libegl1 libgl1-mesa-dri \
  xvfb
```

The second and third lines are exactly what CI installs: windowing libraries for the egui
shell, and Mesa's software Vulkan so the UI tests and `sp screenshot` (which render through
wgpu) have an adapter without a GPU. `xvfb` is only for running the desktop app headless (§5).

Optional: `gh` (GitHub CLI) from <https://cli.github.com/>, then `gh auth login`.

## 4. Toolchain and first build

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
source "$HOME/.cargo/env"

git clone git@github.com:dickhfchan/sheetpro.git ~/projects/sheetpro
cd ~/projects/sheetpro
rustup toolchain install      # installs the nightly pinned in rust-toolchain.toml + clippy, rustfmt

cargo xtask corpus            # fetch the oracle .xlsx files
cargo xtask check             # all offline gates, in CI order
```

`cargo xtask check` passing is the signal the move is done. Judge it by exit code.

## 5. Running the app without a screen

Most work never needs a window:

```sh
cargo run -p sp -- control                       # JSON-lines session on stdin/stdout
cargo run -p sp -- mcp                           # MCP server on stdio
cargo run -p sp -- screenshot --out shot.png --script steps.jsonl
```

`sp screenshot` renders offscreen through lavapipe, so golden rule 5 (verify UI changes
visually) works unchanged: copy the PNG back (`scp server:~/projects/sheetpro/shot.png .`) or
open it from your editor's remote session.

The desktop app (`cargo run -p sheetpro`) and `cargo xtask smoke` open real windows. On a
server, give them a virtual display:

```sh
xvfb-run -a -s "-screen 0 1920x1080x24" cargo xtask smoke
xvfb-run -a cargo run -p sheetpro -- --control-port 7878
```

`cargo xtask package` runs smoke at the end; set `SP_SKIP_SMOKE=1` if you have no display at
all. To *see* the app interactively, use X11 forwarding (`ssh -X`) or a VNC/RDP desktop on the
server. Expect it to be slow: rendering is in software.

The control port binds to `127.0.0.1` only. To drive it from your laptop, tunnel it rather
than exposing it: `ssh -L 7878:127.0.0.1:7878 server`.

## 6. Claude Code on the server

```sh
curl -fsSL https://claude.ai/install.sh | bash
claude          # log in once
```

Claude Code keys project memory by the absolute path of the repo, with `/` replaced by `-`.
Moving from `/Users/<you>/projects/sheetpro` to `/home/<you>/projects/sheetpro` changes the
folder name, so copy the memory into the new slug:

```sh
# on the server, after copying the old folder over
mkdir -p ~/.claude/projects/-home-$USER-projects-sheetpro
cp -r ~/old-claude/projects/-Users-<you>-projects-sheetpro/memory \
      ~/.claude/projects/-home-$USER-projects-sheetpro/
```

Memories that name macOS paths (e.g. `~/Library/...`) are worth a quick edit.

Run long agent sessions inside `tmux` so they survive a dropped SSH connection. Parallel
agents follow AGENTS.md §6 unchanged (`CARGO_TARGET_DIR=target/agent-<name>`); watch disk
space, since each build dir is tens of GB, and delete them when done.

## 7. Platform differences to expect

- **Settings folder**: `$XDG_CONFIG_HOME/sheetpro` (usually `~/.config/sheetpro`) instead of
  `~/Library/Application Support/SheetPro`; `SHEETPRO_HOME` overrides both.
- **Packaging**: `cargo xtask package` produces `dist/sheetpro-<version>-linux-<arch>/`, not
  the macOS `.app` and zip. Build macOS trial bundles on a Mac (or CI's macOS runner).
- **Perf numbers**: `perf/budgets.toml` is shared, but absolute timings differ by machine.
  When recording before/after numbers in the devlog, compare runs on the same server.
- **Fonts and rendering**: the app uses egui's bundled fonts, so screenshots should match
  across platforms; software rendering may differ in anti-aliasing at the pixel level.

## 8. Checklist

- [ ] Old machine: nothing unpushed, no stashes, worktrees cleaned up
- [ ] Server: packages from §3 installed
- [ ] `rustup toolchain install` picked up the pinned nightly
- [ ] `cargo xtask corpus` then `cargo xtask check` exit 0
- [ ] `sp screenshot` writes a PNG that looks right
- [ ] `xvfb-run -a cargo xtask smoke` passes (if you use smoke)
- [ ] Claude Code installed, memory copied to the new path slug
- [ ] Git identity, SSH key and `gh` login set up on the server
