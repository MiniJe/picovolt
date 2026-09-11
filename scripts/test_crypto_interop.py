"""Independent XChaCha20-Poly1305 interoperability using PyNaCl/libsodium.

Uses generated test-only files and an explicit test key. No production secrets.
Run with a compiled 2.2 pv executable: python scripts/test_crypto_interop.py <pv>
"""
from pathlib import Path
import os
import subprocess
import sys
import tempfile
from nacl.bindings import crypto_aead_xchacha20poly1305_ietf_decrypt as decrypt
from nacl.bindings import crypto_aead_xchacha20poly1305_ietf_encrypt as encrypt

cli = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix='pv-crypto-interop-') as directory:
    root = Path(directory)
    key = bytes(range(32))  # Deliberately public test fixture; never deploy this key.
    keyfile = root / 'key'
    keyfile.write_bytes(key)
    keyfile.chmod(0o600)
    vault = root / 'vault'
    subprocess.run([cli, 'vault', 'create', str(vault), '--key-file', str(keyfile)], check=True, capture_output=True)
    original = vault.read_bytes()
    plaintext = decrypt(original[64:], original[:64], original[32:56], key)
    assert plaintext.startswith(b'PVDB')
    header = bytearray(original[:64])
    header[32:56] = os.urandom(24)
    independent = bytes(header) + encrypt(plaintext, bytes(header), bytes(header[32:56]), key)
    candidate = root / 'libsodium-envelope'
    candidate.write_bytes(independent)
    subprocess.run([cli, 'crypto', 'verify', str(candidate), '--key-file', str(keyfile)], check=True, capture_output=True)
    altered = bytearray(independent)
    altered[-1] ^= 1
    candidate.write_bytes(altered)
    rejected = subprocess.run([cli, 'crypto', 'verify', str(candidate), '--key-file', str(keyfile)], capture_output=True)
    assert rejected.returncode != 0
print('Passed PicoVolt-to-libsodium decryption, libsodium-to-PicoVolt verification, and tamper rejection.')
