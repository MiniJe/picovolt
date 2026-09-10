# Local Hub release preparation

This is a working local packaging tool, not a hosted service. It reuses PicoVolt
2.0's signature verification and inspection. Install Python 3.10+ and a `pv` CLI
built with `--features data-tools`.

```sh
pv bake ./data catalog.pvdb
pv dataset keygen publisher.secret
# Keep the private key private; independently pin the printed public key.
pv dataset sign catalog.pvdb --key publisher.secret --name catalog@2026-09-10 --output catalog.signed.json
python scripts/hub_release.py catalog.pvdb catalog.signed.json --dataset catalog --release 2026-09-10 --public-key TRUSTED_PUBLIC_KEY_HEX --output ./artifacts/catalog-2026-09-10
```

`--pv` selects an explicit CLI path. The output contains `dataset.pvdb`, its
signed manifest and `release.json` with transport SHA-256 digests and lengths.
Inputs are copied into a private staging directory before verification. A failed
check produces no completed output; an existing output is never overwritten.
Keep the output parent access-controlled and do not modify completed bundles.

The signed name must match `dataset@release`. Consumers must independently pin
the expected identity and trusted public key and use `pv dataset verify` before
opening. The public key in `release.json` is informational, not a trust anchor.
The descriptor itself is unsigned; hashes alone do not authenticate downloads.
Do not treat this as a full rollback/replay-resistant client protocol.

No network calls, credentials, payments or private keys are used by the tool.
Uploading, private delivery, release registries and channel promotion remain the
next [Hub milestones](HUB_ROADMAP.md).
