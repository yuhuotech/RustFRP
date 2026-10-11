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

Generate the signing key outside the repository with OpenSSL, put the private
PEM in the protected GitHub secret, and pin its public key in the installer and
updater before distributing signed releases. The attached public key is useful
for verification only after it has been trusted through a separate channel.

Verify downloaded files with:

```sh
bash tools/release/verify.sh /path/to/trusted-public-key.pem /path/to/release
```

The release workflow requires those secrets and fails closed when they are
missing. It does not publish unsigned packages.
