# juicefront

Web UI wrapper for [Juicebox²](https://github.com/juiceboxdev/juicebox). Spawns the Astro + SolidJS frontend and proxies API and storage requests to the backend services.

## How it works

Detects `bun` or `npm`, syncs the workspace version from `Cargo.toml` into `ui/package.json`, then:

- **Debug mode**: runs `astro dev` (hot reload)
- **Release mode**: runs `astro build` then serves with `node dist/server/entry.mjs`

Listens on port 6400 by default.

## Proxy routes

| Path | Target |
|------|--------|
| `/upload` | juiceback (port 6401) |
| `/file/` | juiceback (port 6401) |
| `/api/` | juiceback (port 6401) |
| `/f/` | juicehost (port 6402) |

## Frontend

The UI lives in `ui/` and is an Astro 7 project with SolidJS components. Supports English, French, Russian, and Spanish. Includes a no-JS fallback mode for each page.

## Pages from static files

Two pages read their content from text files in `ui/public/`, letting you customize them without editing the Astro templates:

- `/terms` reads from `terms-of-service.txt` (template at `terms-of-service.example.txt`)
- `/privacy` reads from `privacy-policy.txt` (template at `privacy-policy.example.txt`)

The `.example.txt` files are templates with `[bracketed placeholders]`. Copy one to the corresponding `.txt` file and fill in your details.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
