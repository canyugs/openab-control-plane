* _2026-09-09 23:34:18 (GPT-6/default)_

# Git snapshot execution boundary probe

Real benign synthetic repo probe at /var/folders/sd/lyvwlbld52j4b4bptd8yfr9w0000gn/T/ocp-git-boundary-probe-v64guhkz configured core.fsmonitor to a host-authored helper that only wrote an external marker and exited1. Calling delivered candidate _validate_clean_repo(repo) executed that helper (marker exists). Clearing global/system config alone does not disable executable repo-local Git configuration. Existing no-project-hooks/no-source-derived-host-execution requirement E-11 therefore remains unproven. Disable or refuse repo-configured execution paths (at least fsmonitor; inspect clean/process filters and recursive submodule/status operations) or use nonexecuting plumbing. Do not trust a source repo's local execution configuration. Keep read-only behavior and add a red regression with a benign marker, not real external services.

Positive control: tracked hidden.py with export-ignore was correctly listed as missing:hidden.py and source_complete false. Do not report that as a defect; preserve that explicit completeness behavior.

Self-check: One real bounded host-execution defect and one passing completeness control recorded; no unrelated security scope added.
