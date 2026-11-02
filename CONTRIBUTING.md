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

Read `CLAUDE.md` (how the project works and its rules) and `docs/DECISIONS.md`. In short: no
`unsafe` and no dead code (the compiler forbids them); every security or reversibility claim has a
test that would fail if it broke; `docs/` changes with the behaviour it describes.
