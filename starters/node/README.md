# PicoVolt Node starter

Requires Node.js 22.12 or newer. The starter pins the PicoVolt version that was
verified with this source release.

```sh
npm ci
npm start
```

The example uses PicoVolt's synchronous SQLite-inspired adapter, prepared
statements, schema defaults, checks, and ordered queries. The underlying
database is an in-memory WebAssembly instance; browser durability is
demonstrated by the adjacent `browser` starter.
