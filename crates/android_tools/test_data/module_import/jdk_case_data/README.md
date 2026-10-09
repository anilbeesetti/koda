Versioned Java source-set lowercasing data

The compressed original UnicodeData.txt and SpecialCasing.txt files are exact
Unicode Consortium data; decompressed SHA-256, size and original URL are in
manifest.json. LICENSE-UNICODE retains their Unicode License v3 permission.
These are data fixtures, not a non-Rust runtime dependency. Runtime casing logic
and generated tables are Rust. No OpenJDK implementation code is copied.

Supported captured Java releases map to the Character documentation's Unicode
baseline: JDK 17–18 → 13.0, 19 → 14.0, 20–21 → 15.0, 22–23 → 15.1,
24–25 → 16.0. Sources: https://docs.oracle.com/en/java/javase/17/docs/api/java.base/java/lang/Character.html
and the same versioned Character API paths for releases 18 through 25.
UnicodeData simple lowercase entries and unconditional SpecialCasing lowercase
entries generate complete per-version lower tables. Combining classes and
Greek letter categories also come from those exact files. Host Rust and ICU
Unicode versions do not select this data.

Conditional Turkish, Azeri and Lithuanian mappings independently implement the
Unicode SpecialCasing conditions. Java String's final sigma behavior uses Java
locale word boundaries. The Rust implementation supports Greek/ASCII alphabetic
words and combining marks in U+0300–U+036F; other sigma-containing contexts are
explicit unavailable until Java-equivalent word breaking is ported. This is
tracked incomplete behavior, not test-parity credit or a wildcard match.

Missing, unsupported or unparseable captured Java versions cannot fold non-ASCII
selection tokens or source names. Ordinary ASCII and unfiltered requests remain
usable. An unavailable selection retains Basic import facts, raw captures and
previous same-owner Kotlin settings. Official JVM differentials and all new Rust
tests remain pending until Root executes them.

The Unicode-named publication tests construct supplemental adapter inputs by
mutating one checked component only after a valid Basic Main model is parsed.
The current Basic model accepts ASCII component names; its restriction remains
unchanged. These tests do not establish end-to-end Unicode-named Android import,
which remains tracked as incomplete applicability/production integration work.

Supported sigma words also admit leading standalone U+0300–U+036F enclosing
marks. They do not count as a preceding cased word letter. After the first real
Greek/ASCII letter, embedded U+0345 counts as cased as required by Java's rule.
First/last cased word positions are computed in one input pass; the large-mark
regression counts the actual context probes and preserves the supported output.
Complex punctuation/dictionary word contexts remain unavailable and unported.
