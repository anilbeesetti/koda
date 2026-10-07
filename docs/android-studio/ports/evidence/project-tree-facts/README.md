These are exact captured validation artifacts for the source snapshot recorded
in `review-source-lead-correction6.json`. `manifest.json` binds their bytes and
hashes. Cargo ran before source commit `0f28d7c178`, and that commit was checked
to contain the same source and fixture bytes. No later commit is retroactively
claimed as the HEAD used by those commands.

The captured Gradle console logs contain a trailing space on the daemon-stop
line. Their bytes remain unchanged. The local `.gitattributes` rule excludes
only these raw `*.log` artifacts from whitespace diagnostics; source and report
files retain normal checks. Full original fixtures and reference notices also
retain their bytes, including intentional end-of-file blank lines.

The initial compile, lint, supplemental Gradle configuration and incorrect new
probe expectation failures are retained. The latter is explicitly flagged in
the task report with its pinned-source and actual-model evidence. All five
original integration cases remain unported, and these artifacts grant no new
upstream parity credit.
