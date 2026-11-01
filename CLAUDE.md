# duw

Disk usage viewer (Rust, egui) that sizes files by allocated blocks, not logical length.

- After every code change: run `cargo test --release`, then `./install.sh` to rebuild and install `/Applications/duw.app`.
- `DUW_SCREENSHOT=out.ppm [DUW_SCREENSHOT_THEME=light|dark] target/release/duw <folder>` saves the window after the scan and quits; use it to check UI changes (convert with `sips -s format png`).
- `git status` hides untracked files on this machine (`status.showUntrackedFiles=no`); `git add` new files explicitly.
