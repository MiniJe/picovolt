/*
 * PicoVolt C ABI.
 *
 * Matches src/ffi.rs (Cargo feature "capi"). Build the shared library with:
 *
 *     cargo build --release --features capi
 *
 * then link against target/release/{libpicovolt.so | picovolt.dll |
 * libpicovolt.dylib} and include this header.
 *
 * Conventions:
 *   - All strings are UTF-8.
 *   - Fallible calls return NULL (or 0) and record a message retrievable on the
 *     same thread with pv_last_error().
 *   - A PvDb handle is NOT thread-safe; do not share one across threads without
 *     external synchronization.
 *   - Panics never cross this boundary; they surface through pv_last_error().
 */
#ifndef PICOVOLT_H
#define PICOVOLT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque database handle. Allocate with pv_open_*, free with pv_close. */
typedef struct PvDb PvDb;

/* Library version, e.g. "0.4.0". Static; never NULL; do not free. */
const char *pv_version(void);

/*
 * Most recent error on the calling thread, or NULL if none. Owned by PicoVolt
 * and valid only until the next PicoVolt call on this thread; copy it to keep
 * it. Do not free.
 */
const char *pv_last_error(void);

/* Open a new, empty in-memory database. NULL only on allocation failure. */
PvDb *pv_open_memory(void);

/* Open a development workspace at path (UTF-8). NULL on error. */
PvDb *pv_open_dev(const char *path);

/* Open a baked .pvdb production monolith at path (UTF-8), read-only. NULL on
 * error. */
PvDb *pv_open_prod(const char *path);

/*
 * Run one SQL statement. Returns a newly allocated, NUL-terminated JSON string
 * the caller must free with pv_string_free(), or NULL on error. Shape:
 *   {"columns":[...],"rows":[[...]]} | {"mutated":n} | {"done":true}
 */
char *pv_query(PvDb *db, const char *sql);

/*
 * Like pv_query but binds `?` placeholders to a JSON array of parameters, e.g.
 * "[1, \"alice\", null]". Each element maps to a value (null, boolean as 0/1,
 * integer, fractional number as decimal, or string) and is substituted as a
 * safely-escaped SQL literal. Returns the JSON result (free with
 * pv_string_free), or NULL on error.
 */
char *pv_query_params(PvDb *db, const char *sql, const char *params_json);

/*
 * Import a SQL dump (e.g. `sqlite3 db .dump`). Returns a JSON report
 * "{\"executed\":n,\"skipped\":[...],\"errors\":[...]}" (free with
 * pv_string_free), or NULL on error.
 */
char *pv_import_sql(PvDb *db, const char *dump);

/* Most recently committed transaction id (the upper bound for BEFORE tx). */
uint64_t pv_current_tx(const PvDb *db);

/*
 * Export the database as a .pvdb byte image. On success returns a buffer of
 * *out_len bytes (free with pv_bytes_free) and writes its length to out_len;
 * returns NULL on error.
 */
uint8_t *pv_export(PvDb *db, size_t *out_len);

/* Import a database from a .pvdb byte image. NULL on error. */
PvDb *pv_import(const uint8_t *bytes, size_t len);

/* Free a string returned by pv_query. NULL is ignored. */
void pv_string_free(char *s);

/* Free a buffer returned by pv_export; pass the same length. NULL is ignored. */
void pv_bytes_free(uint8_t *ptr, size_t len);

/* Close and free a database handle. NULL is ignored. */
void pv_close(PvDb *db);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* PICOVOLT_H */
