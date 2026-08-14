# Runtime sources

- macOS: IINA 1.4.4 release image, verified by `scripts/prepare-macos-libmpv-dev.mjs`.
- Windows: zhongfly/mpv-winbuild pinned archive, verified by `scripts/prepare-windows-libmpv-dev.mjs`.
- Linux: distribution `libmpv1` or `mpv-libs` package.

Each staged Windows/macOS runtime includes `SOURCE.json` with the exact archive digest and target architecture.
