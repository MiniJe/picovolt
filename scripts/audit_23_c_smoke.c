/* Native C ABI audit smoke. Compile against the matching 2.3 library:
 * cc -Iinclude scripts/audit_23_c_smoke.c -Ltarget/debug -lpicovolt \
 *    -Wl,-rpath,target/debug -o /tmp/pv23-c-smoke
 */
#include "picovolt.h"
#include <stdio.h>
#include <string.h>

static int run(PvDb *db, const char *sql) {
    char *result = pv_query(db, sql);
    if (!result) {
        fprintf(stderr, "%s\n", pv_last_error());
        return 0;
    }
    pv_string_free(result);
    return 1;
}

int main(void) {
    PvDb *db = pv_open_memory();
    if (!db) return 1;
    if (!run(db, "CREATE TABLE t (id)") || !run(db, "INSERT INTO t VALUES (42)")) {
        pv_close(db);
        return 2;
    }
    PvStmt *stmt = pv_prepare(db, "SELECT id FROM t");
    if (!stmt || pv_stmt_parameter_count(stmt) != 0) {
        if (stmt) pv_stmt_close(stmt);
        pv_close(db);
        return 3;
    }
    char *result = pv_stmt_execute(stmt, db, "[]");
    if (!result || !strstr(result, "42")) {
        if (result) pv_string_free(result);
        pv_stmt_close(stmt);
        pv_close(db);
        return 4;
    }
    pv_string_free(result);
    pv_stmt_close(stmt);
    result = pv_query(db, "SELECT FROM");
    if (result || !pv_last_error()) {
        if (result) pv_string_free(result);
        pv_close(db);
        return 5;
    }
    pv_close(db);
    pv_close(NULL);
    puts("C ABI smoke passed");
    return 0;
}
