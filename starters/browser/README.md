# Browser starter

This is a Vite application, not a standalone file. Browsers assign `file://`
pages unique origins and block their ES-module imports; OPFS also requires a
secure HTTP origin. The included Vite configuration also handles wasm-pack's
WebAssembly module import.

```sh
npm ci
npm run dev
```

Vite opens the correct localhost URL. For a production check, run `npm run build`
followed by `npm run preview`. Each reload records the page visit through one
reusable prepared statement, persists it to OPFS, and shows the latest ten rows.
