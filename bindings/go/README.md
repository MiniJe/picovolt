# PicoVolt for Go

Go bindings for the [PicoVolt](https://github.com/MiniJe/picovolt) embedded
database engine, via its C ABI and `cgo`.

These bindings expose what PicoVolt is good at: a single-writer, embedded engine
with SQL, MVCC time-travel, and a compile-to-single-file (`.pvdb`) path. They do
not add concurrent writers, so this is for embedded use
(CLI tools, desktop apps, local caches, an embedded analytics store), not as a
drop-in for a concurrent web backend's primary database.

## Requirements

- A C toolchain (`cgo` is required): GCC/Clang on Linux/macOS, MinGW-w64 on
  Windows.
- The PicoVolt native C ABI library matching the Go module version. The Go
  module already contains `include/picovolt.h`; it never reads a header or
  library from a PicoVolt source checkout.

Download `picovolt-capi-<platform>.tar.gz` from the matching PicoVolt GitHub
Release and extract it to a directory of your choice. Each archive contains
`include/picovolt.h` and `lib/` with the library built from the same release
commit. You can instead build that library from a PicoVolt source checkout:

```sh
cargo build --release --features capi
```

This writes `target/release/libpicovolt.so` (Linux),
`target/release/libpicovolt.dylib` (macOS), or `target/release/picovolt.dll`
(Windows). MinGW also needs the matching import library at link time. PicoVolt
does not silently fall back to `target/release`; provide the native directory
explicitly or install the library in a standard compiler and loader search path.
The Windows release archive includes the MSVC import library when Rust produces
one; a MinGW toolchain that cannot consume it must build the GNU-target C ABI
library from source.

## Linking and running

Set `PICOVOLT_NATIVE_DIR` to the extracted release bundle or the build output,
then expose it at both link time and run time.

Linux:

```sh
export PICOVOLT_NATIVE_DIR=/path/to/picovolt/lib
export CGO_LDFLAGS="-L$PICOVOLT_NATIVE_DIR"
export LD_LIBRARY_PATH="$PICOVOLT_NATIVE_DIR:$LD_LIBRARY_PATH"
go test ./...
```

macOS:

```sh
export PICOVOLT_NATIVE_DIR=/path/to/picovolt/lib
export CGO_LDFLAGS="-L$PICOVOLT_NATIVE_DIR"
export DYLD_LIBRARY_PATH="$PICOVOLT_NATIVE_DIR:$DYLD_LIBRARY_PATH"
go test ./...
```

Windows PowerShell:

```powershell
$PICOVOLT_NATIVE_DIR = "C:\path\to\picovolt\lib"
$env:CGO_LDFLAGS = "-L$PICOVOLT_NATIVE_DIR"
$env:PATH = "$PICOVOLT_NATIVE_DIR;$env:PATH"
go test ./...
```

Once the module is tagged, a clean application install is:

```sh
go get github.com/MiniJe/picovolt/bindings/go@vX.Y.Z
```

The native library remains a separate, checksummed and provenance-attested
release artifact; `go get` supplies the wrapper and its matching header. For
deployment, install the library in a platform search path, set an rpath, or ship
it beside your executable according to your operating system's loader rules.

## Usage

```go
db, err := picovolt.OpenMemory()
if err != nil {
    log.Fatal(err)
}
defer db.Close()

db.Query("CREATE TABLE users (id, name)")
db.Query("INSERT INTO users VALUES (1, 'alice')")

rows, _ := db.Query("SELECT * FROM users")
fmt.Println(rows) // {"columns":["id","name"],"rows":[[1,"alice"]]}
```

Prepare repeated SQL once to cache its text and positional-parameter count:

```go
insert, err := db.Prepare("INSERT INTO users VALUES (?, ?)")
if err != nil {
    log.Fatal(err)
}
defer insert.Close()

if _, err := insert.Execute(1, "Ada"); err != nil {
    log.Fatal(err)
}
if _, err := insert.Execute(2, "Lin"); err != nil {
    log.Fatal(err)
}
```

`ParameterCount` reports the exact arity. `Execute` JSON-encodes values and
passes them through the same injection-safe binder as `QueryParams`; blobs are
unsupported. Preparation validates SQL and retains a native statement handle;
close it before closing the database. The statement does not own or close its
database. Schema errors are reported on execution.

`Query` returns the result as a JSON string
(`{"columns":[...],"rows":[[...]]}` / `{"mutated":n}` / `{"done":true}`);
decode it with `encoding/json`. Other entry points: `OpenDev`, `OpenProd`,
`Import`, `Export`, `Begin`, `Commit`, `Rollback`, `InTransaction`, `CurrentTx`,
and `Version`. The `pvsql` subpackage provides a `database/sql` driver.
Its reusable `sql.Stmt` now reports exact placeholder arity to `database/sql`.
