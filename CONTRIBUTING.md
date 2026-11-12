# Contributing to TorqOS

TorqOS is licensed under the [Apache License 2.0](LICENSE). By contributing, you agree that your
contribution is licensed under it too (the licence's section 5).

## Sign off every commit (DCO)

Each commit carries a sign-off: your certification, under the [Developer Certificate of Origin
1.1](DCO), that you wrote the change or otherwise have the right to submit it under the project's
licence. Add it with `-s`:

```
git commit -s -m "torqcli: …"
```

which appends a line with your name and email, as git knows them:

```
Signed-off-by: Your Name <you@example.com>
```

Use your real name (no pseudonyms or anonymous contributions). A pull request whose commits are not
all signed off is not merged; `git commit --amend -s` or `git rebase --signoff` adds it to commits
already made.

## Before you open a pull request

Read the [README](README.md), [docs/VISION.md](docs/VISION.md) and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) first, then `docs/DECISIONS.md` for the reasons behind a design. (`CLAUDE.md` is written for AI coding sessions working in the repository, and holds the same rules.) In short: no
`unsafe` and no dead code (the compiler forbids them); every security or reversibility claim has a
test that would fail if it broke; `docs/` changes with the behaviour it describes.

## Releasing (maintainers)

A release is a tag on `main`; `.github/workflows/release.yml` does the rest (ADR-028): it checks the
tag is the source's version, runs every test, builds the images and the ISO (x86_64 only: aarch64 is not built),
and only then publishes the source to `torqosorg/torqos-releases` (signed, tagged, `stable` moved)
and a GitHub Release with the files and their signed checksums.

```sh
scripts/bump-version 0.3.0 --release      # bump and commit if needed, check, ask, tag v0.3.0, push
scripts/bump-version 0.3.0-rc1 --release  # a pre-release of 0.3.0 first
```

`--release` refuses before it changes anything unless: you are on `main` with nothing uncommitted,
`main` is GitHub's, CI has not failed there, the tag is not taken, and a stable version is newer
than the last stable tag. It then shows what it will push and asks you to type the tag (`--yes`
skips the question); a wrong answer undoes its version commit. Without `--release`,
`scripts/bump-version 0.3.0 [--commit]` only sets the version, for a release made by hand
(`git tag -a v0.3.0 -m "TorqOS v0.3.0" && git push origin v0.3.0`).

A tag with a hyphen (`v0.3.0-rc1`) is a pre-release: everything runs and is published, but `stable`
does not move, so no machine updates to it. The source carries `0.3.0`; `-rc1` is the tag's alone.
A tag that is not the source's version is refused before anything runs. A release that fails is not
retagged: fix it and tag the next patch version.
