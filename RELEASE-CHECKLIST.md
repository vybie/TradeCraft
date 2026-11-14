# Release coordination

## Next release: 0.3.0 (planned)

Status as of November 14, 2026: the 0.3.0 alpha is published for Windows, Apple Silicon and Intel macOS. Both Windows editions were rebuilt. The Mac packages were built and tested on GitHub runners; interactive validation on a Mac remains outstanding.

- [ ] Reconcile the completed work across the TradeCraft chats and the final source. Update `CHANGELOG.md`, draft 0.3.0 release notes and verify known limitations. In particular, reconcile the chart timezone description with the latest Pine behavior and the user's local/New York preference.
- [ ] Review the final diff, untracked files and branch history; preserve all local egui-charts changes. Review the tracked upstream revision and scheduled monitor, including relevant compatibility and licensing changes.
- [ ] Run the dependency license audit for Windows community and personal and both macOS targets on the final locked dependency graph. Review changed dependencies and actual notices; do not update the baseline without review. Resolve or explicitly document remaining distribution and provenance issues in `ARCHITECTURE.md`, `ATTRIBUTION.md` and `licenses/upstream/README.md`.
- [ ] Run appropriate tests and build the community Windows package using `scripts/build-community.ps1`.
- [ ] Build the personal Windows executable using `scripts/build-personal.ps1` and verify the stable `target/personal/tradecraft.exe` was updated. A running app must be fully quit and reopened to load it.
- [ ] Test the published macOS apps interactively, including Keychain, persistence and restart behavior. Apple Silicon and Intel runner builds and automated tests passed; builds are unsigned and not notarized.
- [ ] Verify packages, included notices, versions and release notes against the same final source before publishing. Follow the project's review and release process.

### Latest audit

On November 14, 2026, both Windows 0.3.0 release builds succeeded. The pinned personal executable was updated and the community package was generated. A running personal app must be fully quit and reopened to load its build. The GitHub macOS workflow built and checked both architectures from `2376167`; both packages passed ZIP and executable-architecture checks and were uploaded to the 0.3.0 release.

On November 13, 2026, `scripts/license-audit.py` passed its graph and notice-availability checks for both Windows configurations (278 packages each). This is not a legal compliance determination; the documented provenance and distribution questions remain open.

Both strict macOS target checks reported missing license texts for `block2 0.6.2`, `dispatch 0.2.0`, `objc2 0.6.5`, `objc2-encode 4.1.0`, and `objc2-app-kit`, `objc2-core-foundation`, `objc2-core-graphics`, `objc2-foundation`, `objc2-metal` and `objc2-quartz-core` 0.3.2. See `licenses/upstream/README.md` for the evidence and unresolved provenance. At the user's request, the macOS build records missing notices as warnings and includes the inventory; the separate audit remains strict.

The exact objc2 source revisions are now accessible and confirm their stated license choices, but full copyright notices for the newer packages are still unverified. Upstream also flags an Apple SDK-derived code distribution question. Do not treat the macOS notice or distribution review as complete until these and the `dispatch` notice are resolved.

An initial GitHub Mac run failed the market-data settings UI test in the full suite. Both architectures passed it when run separately and passed the remaining provider and UI tests; the earlier failure's cause and real Mac interaction still need verification. The separate test reports failures without blocking packaging, while the remaining tests gate the macOS build.

The 0.3.0 release notes are in `RELEASE-NOTES-0.3.0.md`; verify them against the final artifact before publishing.
