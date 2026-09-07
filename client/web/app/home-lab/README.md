# Home UI lab

Run `npm run dev:home-lab`, then open
**http://localhost:1421/home-lab.html**. The usual backend must run on port 8765;
`npm run dev` already provides it.

Current renders production. Proposed starts from the same UI through this
folder’s `DashboardRoute.tsx`. The approved changes have been promoted, so both
views intentionally match.

For the next experiment:

1. Replace the local `DashboardRoute.tsx` re-export with a copy of
   `client/web/pages/home/DashboardRoute.tsx`.
2. Copy any components you want to change into this folder and update the copied
   imports. Keep production components and shared styles untouched during comparison.
3. Compare desktop, tablet, phone, and both themes. Use **Full size** to judge
   actual typography; side-by-side frames scale to fit.
4. Once approved, promote the changes, restore the re-export, and remove temporary
   component copies.

Both previews use the live backend; their actions affect the real workspace.
Routes are shared with production to prevent drift.

Validate with `npm run build`, `npm run build:home-lab`, `npm run lint`, and
`npm test`. Lab output goes to `build/home-lab`; production uses `dist` and excludes
the comparison entry.
