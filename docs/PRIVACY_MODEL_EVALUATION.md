# Local screening: measured limits

The optional `meeting-v2` policy combines three fixed image models: the
Falconsai sensitive-content classifier, Google SigLIP 2 and Marqo FashionCLIP.
Their quantized ONNX files, preprocessing, generic text vectors, thresholds,
immutable source revisions and SHA-256 hashes are recorded in
[`third_party/privacy-models.json`](../third_party/privacy-models.json).
The selected models use Apache-2.0 terms; ONNX Runtime 1.31.1 uses MIT with
retained third-party notices. See the [model notices](../third_party/privacy-model-NOTICE.md).
Weights and the native runtime are downloaded only when requested. Python,
remote custom model code and an external screening API are not used.

All four sensitive checks must clear in both context encoders, together with
the sensitive classifier. A swimwear result never overrides a blocked signal.
The full image is letterboxed, preserving subjects near its edges.
Unscanned, uncertain and failed results do not automatically pass. Explicit
Allow is bound to verified image content; Personal only and Never take precedence.

The frozen policy was checked on 72 fresh public reference photographs,
separate from calibration. Exact aggregate results and limitations are in
[`privacy-reference-evaluation.json`](../third_party/privacy-reference-evaluation.json).

| Category | Samples | Automatically admitted |
| --- | ---: | ---: |
| Ordinary | 30 | 3 |
| Swimwear | 19 | 2 |
| Sleepwear | 6 | 0 |
| Underwear | 13 | 0 |
| Bathing | 4 | 0 |

There were zero execution errors and no observed admissions from the blocked
categories in this small set. **False exclusions are substantial.** Nine
low-light ordinary photos and six clothed sleepers all failed to clear. These
results do not estimate personal-library accuracy or establish reliable
handling of tiny, occluded or mixed subjects. Screening can miss details and
is not a safety guarantee. Check the actual desktop before sharing it.

On the tested Intel Omarchy host, a complete cold job had p95 8.52 seconds and
peak process RSS 471,982,080 bytes (about 450 MiB), under a 40% single-core CPU
cap and a 768 MiB test limit. These are measured reference-job results, not
minimum hardware guarantees. The production service caps CPU at 40% and
memory at 1 GiB, shared with its worker. Jobs run sequentially in short-lived
native processes; model residency ends on exit. Cached admitted changes do
not run inference, recrop or regenerate palettes. Both privacy settings off
means no automatic screening.

Photos, full-frame evidence, image-associated decisions and corrections stay
in private local application data. The committed generic text vectors contain
no personal photographs or identities. Originals and Photos person assignments
are untouched. See [Meeting Mode](MEETING_MODE.md) for controls and recovery.
