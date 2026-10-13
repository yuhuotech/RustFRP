# Release signing

The `v*` GitHub Actions workflow builds macOS arm64/x86_64 DMGs and ZIPs,
Windows x86_64 setup/ZIP, Linux x86_64 DEB/tar.gz, and the Flutter Web panel.
It signs the SHA256 manifest with an Ed25519 private key, then publishes the
manifest, signature, and derived public key alongside the packages.

Configure these repository Actions secrets before creating a release tag:

- `RUSTFRP_RELEASE_SIGNING_KEY_PEM`: a long lived, unencrypted Ed25519 PEM
  private key. Restrict access to the release environment and keep a protected
  offline backup.
- `APPLE_CERTIFICATE_BASE64`, `APPLE_CERTIFICATE_PASSWORD`,
  `APPLE_DEVELOPER_IDENTITY`, `APPLE_ID`, `APPLE_APP_PASSWORD`, and
  `APPLE_TEAM_ID`: Developer ID signing and notarization credentials.

Set the repository Actions variable `RUSTFRP_RELEASE_PUBLIC_KEY_HEX` to the
32-byte raw Ed25519 public key in lowercase or uppercase hexadecimal. Every
Agent binary embeds this trusted key and `GITHUB_REPOSITORY`; the publish job
derives the public key from the signing secret and fails unless it matches the
embedded key. Do not rotate the key without a staged release that still trusts
the previous key.

Generate the signing key outside the repository with OpenSSL, put the private
PEM in the protected GitHub secret, and set the matching public-key repository
variable before distributing signed releases. The Agent embeds this key at
build time; installers still need a separately trusted verification key. The
attached public key is useful only after it has been trusted through a separate
channel.

Verify downloaded files with:

```sh
bash tools/release/verify.sh /path/to/trusted-public-key.pem /path/to/release
```

The release workflow requires those signing values and fails closed when they
are missing. Agent updates download the signed `SHA256SUMS`, verify its
Ed25519 signature using the embedded key, verify the selected binary digest,
then replace the Agent with rollback on failure. A Controller administrator
sends the requested version to an online Agent; the Agent exits after a
successful replacement so its process supervisor can start the new binary.

The CA-07 Agent updater and D-09 Flutter desktop App updater are implemented
and have automated signature, digest, install-plan, and rollback tests. The App
updater checks GitHub Releases or the configured mainland mirrors and installs
the package for the detected platform. Neither updater can accept an unsigned
package; both use the embedded Ed25519 public key.

No signed Release has been published or verified with real Actions credentials
yet. Configure `RUSTFRP_RELEASE_SIGNING_KEY_PEM` and the matching
`RUSTFRP_RELEASE_PUBLIC_KEY_HEX` variable before publishing; the workflow checks
that the public key compiled into the Agent/App matches the signing key. Public
website deployment and multi-platform Flutter desktop acceptance remain tasks.
The authorized Linux Controller/Agent deployment, public HTTPS panel and
cross-host Edge migration passed on 2026-10-13; see
`docs/WAN_ACCEPTANCE_20261013.md` for exact network scope and remaining limits.
Those results do not establish a successfully signed Release update.
