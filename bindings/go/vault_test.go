package picovolt

import (
	"bytes"
	"path/filepath"
	"strings"
	"testing"
)

func TestVault(t *testing.T) {
	path := filepath.Join(t.TempDir(), "vault")
	key := bytes.Repeat([]byte{7}, 32)
	vault, err := OpenVault(path, key, false, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = vault.Request(`{"action":"batch","commands":[{"sql":"CREATE TABLE t(id)"},{"sql":"INSERT INTO t VALUES(?)","params":[42]}]}`); err != nil {
		t.Fatal(err)
	}
	if _, err = OpenVault(path, key, false, false); err == nil {
		t.Fatal("second writer admitted")
	}
	newKey := bytes.Repeat([]byte{8}, 32)
	if err = vault.RotateKey(newKey, false); err != nil {
		t.Fatal(err)
	}
	vault.Close()
	if _, err = OpenVault(path, key, false, false); err == nil {
		t.Fatal("old key accepted")
	}
	vault, err = OpenVault(path, newKey, false, false)
	if err != nil {
		t.Fatal(err)
	}
	defer vault.Close()
	result, err := vault.Request(`{"action":"query","sql":"SELECT * FROM t"}`)
	if err != nil || !strings.Contains(result, "42") {
		t.Fatalf("%s %v", result, err)
	}
	if _, err = vault.Request(`{"action":"query","sql":"DELETE FROM t"}`); err == nil {
		t.Fatal("query accepted mutation")
	}
}
