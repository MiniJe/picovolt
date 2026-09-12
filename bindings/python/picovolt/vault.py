"""Native encrypted vaults, PicoVolt 2.2+. All successful batches are persisted."""
import ctypes
import json
import os
from . import _lib, _last_error, PicoVoltError

if hasattr(_lib, 'pv_vault_open'):
    _lib.pv_vault_open.argtypes = [ctypes.c_char_p, ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.c_int32, ctypes.c_int32]
    _lib.pv_vault_open.restype = ctypes.c_void_p
    _lib.pv_vault_request.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    _lib.pv_vault_request.restype = ctypes.c_void_p
    _lib.pv_vault_rotate.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.c_int32]
    _lib.pv_vault_rotate.restype = ctypes.c_int32
    _lib.pv_vault_close.argtypes = [ctypes.c_void_p]
    _lib.pv_vault_close.restype = None

def _secret(key, password):
    if (key is None) == (password is None):
        raise ValueError('Supply exactly one key or password as bytes')
    value = key if key is not None else password
    if not isinstance(value, (bytes, bytearray)):
        raise TypeError('Secret must be bytes or bytearray')
    if key is not None and len(value) != 32 or password is not None and not 12 <= len(value) <= 1024:
        raise ValueError('Keys require 32 bytes; passwords require 12..1024 bytes')
    return (ctypes.c_uint8 * len(value)).from_buffer_copy(value), int(password is not None)

class Vault:
    """Single-writer encrypted file with memory-resident data. Use a private directory.

    Application-owned Python secret objects and result data cannot be guaranteed
    zeroized. Close releases the file lock; no implicit writes occur on close.
    """
    def __init__(self, path, *, key=None, password=None, create=False):
        self._ptr = None
        if not hasattr(_lib, 'pv_vault_open'):
            raise PicoVoltError('Vaults require native PicoVolt 2.2 with encryption')
        path = os.fspath(path)
        if not isinstance(path, str) or '\0' in path:
            raise ValueError('Path must be a string without NUL')
        buffer, mode = _secret(key, password)
        try:
            self._ptr = _lib.pv_vault_open(path.encode('utf-8'), buffer, len(buffer), mode, int(bool(create)))
        finally:
            ctypes.memset(buffer, 0, len(buffer))
        if not self._ptr:
            raise _last_error()
    def _request(self, value):
        if not self._ptr:
            raise PicoVoltError('vault is closed')
        payload = json.dumps(value, allow_nan=False, separators=(',', ':')).encode('utf-8')
        if len(payload) > 1024 * 1024:
            raise ValueError('Vault request exceeds 1 MiB')
        ptr = _lib.pv_vault_request(self._ptr, payload)
        if not ptr:
            raise _last_error()
        try:
            return json.loads(ctypes.string_at(ptr))
        finally:
            _lib.pv_string_free(ptr)
    def query(self, sql, params=None):
        return self._request(dict(action='query', sql=sql, params=[] if params is None else list(params)))
    def batch(self, commands):
        """Commit a list of {sql, params?} commands atomically; SELECT uses query()."""
        return self._request(dict(action='batch', commands=list(commands)))
    def inspect(self):
        return self._request(dict(action='inspect'))
    def retrieve(self, request):
        return self._request(dict(action='retrieve', request=request))
    def backup(self, path):
        return self._request(dict(action='backup', path=os.fspath(path)))
    def rotate_key(self, *, key=None, password=None):
        if not self._ptr:
            raise PicoVoltError('vault is closed')
        buffer, mode = _secret(key, password)
        try:
            if not _lib.pv_vault_rotate(self._ptr, buffer, len(buffer), mode):
                raise _last_error()
        finally:
            ctypes.memset(buffer, 0, len(buffer))
    def close(self):
        if self._ptr:
            _lib.pv_vault_close(self._ptr)
            self._ptr = None
    def __enter__(self):
        return self
    def __exit__(self, *_exc):
        self.close()
    def __del__(self):
        try:
            self.close()
        except Exception:
            pass
