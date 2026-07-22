# Controller protocol

`controller-protocol` is the provider-neutral serialized contract between an
external controller and the OpenAB control-plane runtime. It contains data
types only: no HTTP server/client, authentication, storage, runtime state, or
provider integration.

## Versioning

- Version `1` is the only currently supported version.
- Peers exchange `VersionOffer` and select the highest common version with
  `negotiate_version`.
- Adding an optional field with a default is backward-compatible. Renaming or
  removing a field, changing a discriminant, or changing field meaning requires
  a new protocol version and a mixed-version conformance test.
- `ErrorCode` values and the JSON envelope shape are stable protocol data.
  `message` is explanatory text and must not be parsed by clients.

The golden corpus under `tests/golden/` pins every action and result variant,
the version offer, and all error codes.

## P2/P3 boundary

P2 defines optional `recipient_inputs` because the compatibility fixture corpus
proves that some sessions need target-specific opening instructions. The OCP
adapter rejects non-empty values until P3 implements atomic session creation and
recipient delivery. This fail-closed boundary prevents a controller from
believing inputs were delivered when the runtime ignored them.

No opaque terminal `result_ref` is included in version 1 yet. It should be added
only when the runtime-event/product-projection work demonstrates its required
semantics.
