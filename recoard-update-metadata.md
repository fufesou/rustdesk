# Update metadata issues inherited from `refact/update-metadata`

Reviewed base: `refact/update-metadata` at `b9a3b0bf5`.

These issues are present in the base branch. They are intentionally recorded here instead of being fixed only in `test/update-metadata-3`.

## P2: Legacy macOS DMG updates do not establish RustDesk signer identity

- `src/manual_update.rs::parse_macos_update_args` accepts `--update <file>.dmg` without signed metadata.
- `src/platform/macos.rs::update_from_dmg` validates the candidate bundle identifier, a generic valid code signature, and Gatekeeper acceptance.
- A different accepted Developer ID application can reuse the RustDesk bundle identifier. The administrator prompt does not establish that the candidate was published by RustDesk.

Base-branch correction: require signed update metadata for external DMGs. If legacy compatibility must remain, compare the installed and candidate designated requirements or pinned Team ID before installation.

## P2: Installer payload versions are not bound to the signed metadata version

- `VerifiedUpdateArtifact::version` is discarded by the normal macOS GUI/manual paths before `update_to_verified_dmg`.
- Windows passes the version only for logging; `update_to_verified` verifies hash, size, and Authenticode but not the EXE/MSI product version.
- The macOS root-service path already performs the expected comparison in `update_from_dmg_as_root`, showing the intended invariant.

A correctly signed metadata file can therefore claim version X while its installer contains version Y. Release workflows reduce this risk by checking package versions, but the runtime trust boundary does not enforce it.

Base-branch correction: pass the signed version through every installation path and compare it with `CFBundleShortVersionString`, Windows `ProductVersion`, or MSI `ProductVersion` before launch or replacement.

## P2: Signed offline metadata permits downgrade and replay

- `verify_offline_update_metadata_with_options` derives the expected version from the signed release ID.
- It does not compare that version with the installed application version.
- `published_at` is deserialized but not used for freshness or rollback protection.

Any previously signed DMG and matching sidecars can be replayed to install an older internally consistent release.

Base-branch correction: require the signed offline version to be newer than the installed version, or require an explicit, separately authorized downgrade mode.
