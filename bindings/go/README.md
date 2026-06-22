# PicoVolt for Go

Go bindings for the [PicoVolt](https://github.com/MiniJe/picovolt) embedded
database engine, via its C ABI and `cgo`.

These bindings expose what PicoVolt is good at: a single-writer, embedded engine
with SQL, MVCC time-travel, and a compile-to-single-file (`.pvdb`) path. They do
not add JOINs, transactions, or concurrent writers, so this is for embedded use
(CLI tools, desktop apps, local caches, an embedded analytics store), not as a
drop-in for a concurrent web backend's primary database.

## Requirements

- A C toolchain (`cgo` is required): GCC/Clang on Linux/macOS, MinGW-w64 on
  Windows.
- The PicoVolt shared library, built from the repository root:

  ```sh
  cargo build --release --features capi
  ```

  This writes `target/release/libpicovolt.so` (Linux),
  `target/release/libpicovolt.dylib` (macOS), or `target/release/picovolt.dll`
  (Windows). The `cgo` directives in `picovolt.go` already point at `../../include`
  and `../../target/release`, so `go build` works from this directory.

## Running the example

```sh
cargo build --release --features capi          # from the repo root
cd bindings/go/example

# Linux
LD_LIBRARY_PATH=../../../target/release go run .
# macOS
DYLD_LIBRARY_PATH=../../../target/release go run .
# Windows (PowerShell): put the DLL dir on PATH, then run
$env:PATH = "..\..\..\target\release;$env:PATH"; go run .
```

The dynamic loader needs to find the library at run time. For a real deployment,
install it to a standard library path, set an rpath, or ship it next to your
binary.

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

`Query` returns the result as a JSON string
(`{"columns":[...],"rows":[[...]]}` / `{"mutated":n}` / `{"done":true}`);
decode it with `encoding/json`. Other entry points: `OpenDev`, `OpenProd`,
`Import`, `Export`, `CurrentTx`, and `Version`.

A `database/sql` driver is a natural next step but is not provided yet.
