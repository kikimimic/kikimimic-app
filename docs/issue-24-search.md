# Local search baseline (#24)

`kikimimic_core::search::search(base, folders, query)` returns source paths,
meeting titles/dates and utterance offsets. It searches originals/translations,
titles and compact recording dates. Markdown/text folder scope is explicit,
nonrecursive and skips symlink files. Calls rescan sources, so removal of a
folder from the argument revokes access and edits/deletes require no index rebuild.

Literal Unicode lowercase substring matching supports one/two-character Japanese.
Spelling variants, width variants and synonyms intentionally do not match.
Results are evidence excerpts, not inferred decisions or project associations.
Folder membership is the explicit association mechanism.

Reproduce: `nix develop -c cargo test -p kikimimic-core` and
`nix develop -c cargo clippy -p kikimimic-core --all-targets -- -D warnings`.
The synthetic test covers short Japanese, English proper names, explicit scope,
edits and deletion. No models are needed.

This is an independently reviewable core API increment toward #24 under #20.
UI folder selection, result navigation, persistent preferences, network-blocked
runtime acceptance, realistic corpus recall/latency and meeting-search fixtures
remain unverified/unimplemented. Q&A and advice integration belong to sibling
issues; this change does not implement them. No semantic model or index is added.
