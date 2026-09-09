* _2026-09-10 00:11:25 (gpt-5.6-luna/max)_

# snapshot-guard

Scope was limited to the Git snapshot execution boundary in source commit
`b74b861f291a6e94d239d7a10858dc3b09aa3a0c`. The cited resolution note and JSON
probe were read, along with the relevant `_git`, `_validate_clean_repo`, and
`_archive_files` implementation and the existing model-evaluation tests. No
live GitHub, model, Docker, credential, network, or service access was used.
No commit, push, Agentflow call, or source/test/doc mutation was made.

## Reproduction

The original module was exercised against disposable clean repositories whose
local or worktree-local `core.fsmonitor` pointed to a host-authored helper. The
helper only wrote an external marker and exited 1. The original
`_validate_clean_repo` returned, and the marker existed. This reproduces the
cited E-11 failure.

The neighboring pathway probe against the original module found:

- `filter.<driver>.clean`: helper ran; clean validation returned.
- `filter.<driver>.process`: helper ran; clean validation failed afterward.
- `diff.external`: helper ran during source-packet construction.
- `tar.tar.command`: the standard local built-in tar archive did not run this
  configured command, but the configured archive command remained an unsafe
  repository-local execution setting.

Direct Git controls were also probed without the Python fix: name-only local
config inspection saw the fsmonitor key, while command-line fsmonitor false,
hooks-path `/dev/null`, empty external diff, `--no-ext-diff`, and
`--no-textconv` completed successfully with both markers absent.

## Correction in `git-boundary.patch`

Before every snapshot Git operation, the module invokes Git config listing with
`--local` and `--worktree`, each using `--no-includes --name-only --null
--list`. It retains only ASCII config names, never values, and fails closed with
a generic reason if inspection fails or a repository-local execution setting is
present. Includes are refused before their targets are read.

The refused local configuration paths are:

- `core.fsmonitor`, `core.fsmonitorHookPath`, `core.hooksPath`, `core.pager`,
  `core.sshCommand`, and `core.gitProxy`;
- `filter.*.clean`, `filter.*.smudge`, `filter.*.process`, and
  `filter.*.required`;
- `diff.external`, `diff.*.command`, and `diff.*.textconv`;
- `tar.*.command`, `credential.helper`, `interactive.diffFilter`,
  `pager.*`, `remote.*.uploadpack`, `remote.*.receivepack`, and
  `submodule.*.update`; and
- `include.*` and `includeIf.*` configuration entries.

Every actual Git invocation additionally clears system/global config, disables
optional locks and terminal prompts, uses `--no-pager`, sets
`core.fsmonitor=false`, routes hooks to the null device, clears credential
helpers and external diff, disables submodule recursion and `ext`/`file`
protocols, and uses `--no-ext-diff --no-textconv` for `git diff`. The source
repository, index, worktree, config, and hooks are not modified.

## Verification

- Red-first `python3 -m unittest -v` boundary selection against the original
  temporary module: exit 1; four boundary tests failed and the clean-repository
  control passed. The worktree-local filter selection also failed on the
  original module.
- Patched marker selection, `python3 -m unittest -q` over six boundary tests:
  exit 0, `OK`.
- Full `python3 -m unittest -q test_review_model_evaluation` against the
  patched temporary module: exit 0, 16/16 tests passed.
- `git apply --check git-boundary.patch` against the declared untouched clone:
  exit 0.
- Patch contains only `scripts/review_model_evaluation.py` and
  `tests/test_review_model_evaluation.py`.
- Patch SHA-256:
  `f6b057f48837d6882ad66058dfb4cffc5e23016e9e66adc8dc56c5637f9d4b8d`.

## Remaining concrete risk

The refusal set is deliberately finite. A future Git release could add a new
repository-local configuration key that launches a process and is not covered
by these names; that would require extending the guard or replacing the Git
CLI path with nonexecuting plumbing. The trusted host `git` binary itself is
outside this repository-content boundary. The corrected implementation also
conservatively rejects some otherwise usable local configurations, which is
the intended fail-closed trade-off.

Self-check: exact source commit, patch-only authority, Git snapshot scope, gpt-5.6-luna/max model, basic effort, green result, and red/green evidence contract frozen.
