# Local privacy screening models

Selected models are data-only ONNX image encoders/classifiers. Images stay on the local machine. Fixed generic text embeddings are included in the deployment manifest; they contain no personal images, face embeddings or identities.

- Falconsai/nsfw_image_detection, revision `96cb0d0342c7afb80cab76ecc58b265fa44da256`, Apache-2.0; quantized ONNX conversion by onnx-community at `1ceb3c7fe1e9f3f2507e6df577437f23a9149fd5`.
- Google SigLIP 2 base patch16-224, revision `75de2d55ec2d0b4efc50b3e9ad70dba96a7b2fa2`, Apache-2.0; quantized ONNX conversion by onnx-community at `ba1f3b0843f24bc5417d38e19c37b287d719b2f4`.
- Marqo FashionCLIP, official quantized ONNX at revision `44f4c655124ed71e90cbf528d82f508d69d8a81b`, Apache-2.0. Its custom Python code is not loaded.

The exact immutable download URLs, SHA-256, byte limits, source model cards, preprocessing and thresholds are in `privacy-models.json`. Copyright remains with the respective model authors. The common Apache-2.0 license is supplied in `privacy-models-Apache-2.0.txt`. Conversion/quantization changes are reflected by the distinct artifact hashes and provenance above. The generic text vectors were computed from these model families for the fixed policy vocabulary.

ONNX Runtime 1.31.1 is copyright Microsoft Corporation and contributors, under the supplied MIT license and ThirdPartyNotices. The Linux installer extracts only the standalone native CPU shared library from the official, pinned distribution archive. It does not install Python or its bindings. The Rust `ort` interface is MIT OR Apache-2.0, version 2.0.0-rc.15.

The reference result is conservative and limited: five of 49 allowed examples passed, with no admitted blocked examples in the 72-image set. Small/occluded mixed scenes and personal-library accuracy remain unmeasured. Automatic screening can make mistakes; this is not a suitability guarantee. Models are optional, privacy settings default off, and missing/unscanned/uncertain content is not automatically admitted.
