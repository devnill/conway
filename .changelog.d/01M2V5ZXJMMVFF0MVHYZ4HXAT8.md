### Fixed

- **`/conway.checkpoint.diff <seq>` rendered its preview backwards** — board item `01M2V5ZXJMMVFF0MVHYZ4HXAT8`. The command's `summary` and `docs/plugins/checkpoint.md` both promise a preview of what `rollback <seq>` would change, but the rendering put the rollback's TARGET bytes on the `-` side and the file's CURRENT bytes on the `+` side — the exact inverse. An operator reading it immediately before a rollback (the one moment this command exists for) would conclude the rollback produces precisely the bytes it is about to discard. The `-` side is now the file as it stands, the `+` side is what the rollback would restore, and for a path the model created the preview shows the whole file being removed with a note saying the rollback deletes it.

### Changed

- **Both sides of a `/conway.checkpoint.diff` are now labelled** — `--- <path> (current)` and `+++ <path> (after rollback to seq N)`. The two headers previously carried the same bare path, so nothing on screen disambiguated a direction that is the whole content of the preview.
- **`docs/plugins/checkpoint.md`** gains a worked example of which way round `diff` reads, and the command's `summary` string names the two sides rather than only the operation.
