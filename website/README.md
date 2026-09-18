# CrabBoss Website

Live: https://crabboss.vercel.app/

Official site sources (VitePress). Deployed **manually** to Vercel.

## Develop

Requires Node 20 (pinned in `.nvmrc`):

```bash
cd website
npm install
npm run dev      # local preview with hot reload
npm run build    # static output to .vitepress/dist
npm run preview  # serve the built output
```

## Deploy to Vercel (dashboard, one time)

1. Vercel → Add New → Project → import `sonyarianto/crabboss`.
2. **Root Directory:** `website`.
3. **Framework Preset:** VitePress (output `.vitepress/dist` is also
   pinned in `vercel.json`, so a missing preset can't break the build).
4. Deploy. Note the URL, then add it to the main README + this file.

## Rules for this folder

- This is the canonical **public narrative** (landing, manual). Technical
  truth lives in `../docs/` + code; link out, don't duplicate.
- Screenshots go in `public/` and must be **CrabBoss itself**. Never
  publish anything from `../radioboss/` (third-party reference).
- After deploy, put the live URL at the top of this file.
