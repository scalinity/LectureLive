# LectureLive

A personal macOS app that records a Zoom lecture, streams it to speech-to-text and writes Markdown
notes: Rust core and CLI (`crates/`), Tauri 2 + Svelte 5 desktop app (`apps/desktop/`). The design is
`docs/spec.md`; the build order, gates and findings are `docs/milestones.md`; each milestone's plan is
in `docs/superpowers/plans/`.

## End of every milestone: merge into main and push

A milestone ends when its findings are committed on its branch (`m<N>-<name>`) and the final
review's fixes are in. Then, in this order:

1. **Scan what the push will publish.** The repository is public, and a pushed commit's contents stay
   public even after a later fix. Scan every added line in `origin/main..m<N>-<name>` (skip
   `Cargo.lock` and `package-lock.json`) for credentials (API keys, tokens, `.env` contents) and for
   personal identifiers that serve no function: home paths such as `/Users/<name>/`, email addresses
   other than the commit author's, private URLs and session links. Classify each hit as a real value,
   a test fixture's synthetic value, or code that builds the pattern, and say which.
2. **Redact a real value from history before pushing, and ask first.** Rewriting history is the
   person's decision. When they agree: save the original lines, with their commits and files, to a
   file outside the repository; rewrite in a scratch clone (`git clone --no-local`, then
   `git filter-repo --replace-text` with a literal replacement), never in this checkout; confirm that
   the diff between the old and new tips is only the redacted lines; push; then reset the local
   branches to `origin/main`.
3. **Fast-forward `main`:** `git checkout main && git merge --ff-only m<N>-<name>`. Each milestone
   branch is cut from `main`, so a fast-forward always applies. A merge commit would mean something
   landed on `main` meanwhile, and that is a reason to stop and look.
4. **Push `main`:** `git push origin main`, a normal push. Never force-push unless the person asks.
   Push `main` only; milestone branches never go to the remote.
5. **Delete the milestone branch:** `git branch -d m<N>-<name>`. Once merged it serves no purpose.
   If a redaction changed its hashes, Git no longer sees it as merged: confirm its tip differs from
   `main`'s commit of the same subject only in the redacted lines, then delete it with `-D`.
6. **Say so in the handover:** the commit `origin/main` now points to, and that the next milestone
   branches from `main`.

This step applies even when a milestone's kickoff prompt says to leave the branch unmerged and
unpushed: this file is the later instruction.

To keep the scan short, write paths in docs and plans as `$HOME/…` or relative to the repository,
never as `/Users/<name>/…`.
