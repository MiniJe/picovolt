package picovolt

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestBundledHeaderDeclaresUsedABI(t *testing.T) {
	header, err := os.ReadFile(filepath.Join("include", "picovolt.h"))
	if err != nil {
		t.Fatal(err)
	}
	text := string(header)
	for _, declaration := range []string{
		"typedef struct PvDb PvDb;",
		"typedef struct PvStmt PvStmt;",
		"PvStmt *pv_prepare(",
		"size_t pv_stmt_parameter_count(",
		"char *pv_stmt_execute(",
		"void pv_stmt_close(",
		"char *pv_query_params(",
		"void pv_close(",
	} {
		if !strings.Contains(text, declaration) {
			t.Errorf("bundled C header is missing %q", declaration)
		}
	}
}

func TestBundledHeaderMatchesCanonicalHeaderInCheckout(t *testing.T) {
	bundled, err := os.ReadFile(filepath.Join("include", "picovolt.h"))
	if err != nil {
		t.Fatal(err)
	}
	canonical, err := os.ReadFile(filepath.Join("..", "..", "include", "picovolt.h"))
	if os.IsNotExist(err) {
		// A downloaded Go module is intentionally self-contained and has no
		// access to the parent Rust repository.
		return
	}
	if err != nil {
		t.Fatal(err)
	}
	normalize := func(contents []byte) []byte {
		return bytes.ReplaceAll(contents, []byte("\r\n"), []byte("\n"))
	}
	if !bytes.Equal(normalize(bundled), normalize(canonical)) {
		t.Fatal("bindings/go/include/picovolt.h is out of sync with include/picovolt.h")
	}
}
