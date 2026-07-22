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

## P3 interpreter boundary

Optional `recipient_inputs` exist because the compatibility fixture corpus
proves that some sessions need target-specific opening instructions. The P3
interpreter validates every recipient, persists all audience-scoped opening
messages in the same transaction as session creation, and then uses the normal
durable outbox for delivery. A common `prompt` is the fallback for roster
members without a specific input.

`add_roster`, `close_session`, and `emit_status` also execute through the same
interpreter as in-process callers. Controlled close is denied by default and
requires an explicit runtime policy grant. Transport authentication,
installation grants, action-id replay storage, and the external HTTP action
endpoint remain P4 scope.

No opaque terminal `result_ref` is included in version 1 yet. It should be added
only when the runtime-event/product-projection work demonstrates its required
semantics.
