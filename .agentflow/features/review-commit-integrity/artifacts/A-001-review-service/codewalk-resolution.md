* _2026-09-09 11:57:19 (GPT-6/default)_

# Boundary investigation resolution

Accepted shared discovery/codewalk after independent source inspection of projection, terminal dispatch, sender, and author decision paths. No tests claimed. Only report changed in independent clone. Requirements-resolution.md remains authoritative on failing both verdict types closed. Old outbox payloads and old findings cannot be promoted to verified evidence by target equality alone; design must cover both. Session-target immutability is necessary only at existing recording boundary, no new registry.

Official API contract verified: https://docs.github.com/en/rest/pulls/reviews#create-a-review-for-a-pull-request documents commit_id request and response; omission uses most recent PR commit. No live GitHub mutation.

Advisor triggers: explore skipped because source/API semantics establish the boundary and the smallest durable-proof alternatives can be resolved explicitly in specification; spike skipped because no unknown runtime mechanism requires an experiment before designing (regression reproduction remains red-first implementation work). Security-scan required after code changes because this crosses approval authority. Learn deferred until implementation evidence exists; no separate product recommendation beyond current scope.

Self-check: Source-based acceptance, external contract verified, legacy proof gap retained for design.
