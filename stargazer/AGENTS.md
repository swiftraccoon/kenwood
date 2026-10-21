# stargazer

D-STAR recording, published-file collection and activity archiving.

## Capture and storage

- Keep capture clock-injected and I/O-free. The session shell owns UDP/time;
  filesystem work runs through explicit blocking tasks.
- Preserve all raw sequence, AMBE and slow-data bytes. Only sequence values
  0 through 20 infer gaps; duplicates and invalid observations infer none.
- Use a fresh `AmbeDecoder` for each transmission and conceal inferred gaps.
  No codec arithmetic, tables or tuning changes belong in archive remediation.
- Require positive, representable duration/concurrent-state bounds and checked
  filesystem target components. Local module A-E is application policy; target
  module validation remains the core A-Z domain.
- Reserve the entire artifact stem exclusively. Sync and publish AMBE first,
  optional WAV second and JSON last without replacement. WAV failure is retained
  in metadata; raw failure prevents JSON publication. Preserve the explicit Unix
  directory-sync seam and qualify other platforms' durability.
- Treat `WriteError::MetadataVisible` as an inspect-before-retry result. Never
  describe visible metadata as proof of completed directory sync or power-loss
  survival. Imported recordings retain absent live endpoint/peer metadata.
- Validate every recognized dvrec stream identity and fixed-width header before
  storing it. Filename identities must match the checked log identity.

## Sessions and control

- Retain retiring supervisor handles and typed completion outcomes until joined.
  Endpoint/protocol changes require retirement before replacement.
- Race the complete DNS/auth/connect attempt against stop and its deadline.
  Closed control channels stop processing. Pending filesystem work must finish
  before successful shutdown; do not detach it behind a timeout.
- Recorder/control CLI support requires Unix sockets and an exclusive recorder
  directory lock. Responses describe local session state, not remote slot state.

## HTTP and survey

- Harvest robots rules follow RFC 9309, including agent groups, wildcards,
  end anchors, allow ties and complete disallow for unreachable policy.
- Require an operator User-Agent, bounded responses, at most five redirects,
  sequential downloads with at least two seconds between attempts, and a stop
  after three consecutive failures. Cancellation preserves download pacing.
- Reserve the survey's 30-second minimum poll interval before network I/O,
  including failures/cancellation. Keep resource bounds caller-selected.
- Publish raw snapshots without replacement before parsing. Append complete,
  bounded JSON lines under a file lock; sync before committing seen identities.
  Failed writes attempt truncate-and-sync rollback and retain both errors if it
  fails. Do not silently ignore malformed/truncated JSONL or directory entries.
- Stream growing archive reads and feature export. Preserve exact observed
  text; metadata matching selects candidates without proving byte identity,
  audio quality, service coverage or retention.
- Use controlled local HTTP/UDP fixtures and temporary files for regressions;
  tests must not contact live services or radios. Join or abort fixture tasks.
- All-zero AMBE is not the zero-error FEC fixture; retain the DVSI silence vector
  used by the existing decoder tests.
