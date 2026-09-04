# PicoVolt Node starter

Requires Node.js 20 or newer. The starter pins the PicoVolt version that was
verified with this source release.

```sh
npm ci
npm start
```

The example uses PicoVolt's synchronous `better-sqlite3`-style adapter, prepared
statements, constraints, and limited queries. The underlying database is an
in-memory WebAssembly instance; browser durability is demonstrated by the
adjacent `browser` starter.
