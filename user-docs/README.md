# TorqOS user documentation

The public documentation site, built with [MkDocs](https://www.mkdocs.org/) and the
[Material](https://squidfunk.github.io/mkdocs-material/) theme.

- **Only `pages/` is published.** The design and development documents in `../docs/` are never read
  by this build, so they stay private. Write user-facing pages here; keep internal notes in `docs/`.
- `mkdocs.yml` sets the site's name, theme, colours and the sidebar order (`nav:`). A new page must be
  added to `nav:` to appear in the sidebar.
- `pages/stylesheets/torqos.css` holds the TorqOS colours and fonts.

## Preview locally

With Docker, from this folder:

```sh
docker run --rm -it -p 127.0.0.1:8000:8000 -v "$PWD":/docs squidfunk/mkdocs-material
```

Then open <http://127.0.0.1:8000>. Pages reload as you save. Or with Python:
`pip install -r requirements.txt && mkdocs serve`.

## Build

```sh
docker run --rm -v "$PWD":/docs squidfunk/mkdocs-material build --strict
```

The site is written to `site/` (ignored by git). `--strict` fails on broken links.

## Publishing (Cloudflare Workers)

The site is deployed as static files on Cloudflare Workers; `wrangler.jsonc` points Cloudflare at
the built `site/` folder. In the Cloudflare dashboard, **Workers & Pages → Create → Import a
repository**, pick `netstacks/torqos`, and set:

| Setting | Value |
|---|---|
| Project name | `torqos-docs` (must match `name` in `wrangler.jsonc`) |
| Build command | `pip install -r requirements.txt && mkdocs build --strict` |
| Deploy command | `npx wrangler deploy` |
| Path (advanced settings) | `/user-docs` |

Every push to `main` then rebuilds and publishes the site.
