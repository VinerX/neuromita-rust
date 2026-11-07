# Pinned model source catalog

`catalog.v1.json` is intentionally empty. The catalog is **not** an online
model registry until it contains approved, SHA-256-pinned source revisions.
Do not populate it with guessed checksums or download unpinned weights.

Each record uses the exact runtime plugin ID, immutable revision ID and every
required `(role, relative_path)` declared by the plugin's dynamic install plan.
The following example shows the shape, not a real deployable source:

```json
{
  "schema_version": 1,
  "entries": [
    {
      "plugin_id": "example_plugin",
      "revision": "approved-revision",
      "assets": [
        {
          "role": "weights",
          "relative_path": "model.bin",
          "sha256": "<64 hexadecimal digits from verified binary>",
          "source": {"kind": "https", "url": "https://models.example.org/model.bin"}
        }
      ]
    }
  ]
}
```

`kind` can instead be `local_file` with an absolute `path` to explicitly supplied
model assets. The Rust `AiProductionInstaller` accepts this catalog through
`ModelAssets::Catalog`, verifies its entries against the plugin's install plan,
then sends them through `ModelAssetStore` for immutable staging, checksum
verification and publication. An absent entry or checksum is a blocking error.

User-specific RVC voice weights and reference assets cannot be represented by a
universal official URL. They require user-approved local files and pins.

## Verified direct upstream Silero TTS components

The Silero TTS model packages are downloaded directly from the upstream
`models.silero.ai` host, *not* via Torch Hub. Both blobs were fetched and hashed
in GitHub Actions run `41671018677` (2026-11-03):

| Language | Model | Direct URL | Bytes | SHA-256 |
| --- | --- | --- | ---: | --- |
| Russian | `v5_ru` | `https://models.silero.ai/models/tts/ru/v5_ru.pt` | 145382211 | `7ba04d42340fe0398042eed2e0d12d62e23096d626b1b9feff4dcb1309197ab4` |
| English | `v3_en` | `https://models.silero.ai/models/tts/en/v3_en.pt` | 57194546 | `02b71034d9f13bc4001195017bac9db1c6bb6115e03fea52983e8abcff13b665` |

These hashes are enforced by the dynamic installation plans for
`silero_rvc_cuda` and `silero_rvc_onnx`. The selected `language` determines which
package is required, at `silero/v5_ru.pt` or `silero/v3_en.pt` within the
immutable model revision. The inference adapter now loads this local artifact
through `torch.package.PackageImporter`; it never calls `torch.hub.load` or the
`silero_tts` helper that may fetch a model implicitly.

**Not a complete installable voice revision:** Silero+RVC also requires the
user's RVC voice weights, content encoder, pitch estimator, and possibly an
index. These have different origins and cannot be substituted with the Silero
package. A `catalog.v1.json` entry is valid only once all required asset roles
for the selected plugin/revision have verified sources. The catalog is not
silently populated with partial entries.

A Silero `.pt` is a `torch.package` archive containing serialized Python model
code. Only load trusted, SHA-256 verified packages; this is not a safe format
for arbitrary user-provided untrusted files.
