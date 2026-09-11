import json
import pytest
from picovolt import PicoVoltError, version
from picovolt.vault import Vault

def test_vault_persistence_atomic_errors_rotation_and_restore(tmp_path):
    assert version() == '2.2.0'
    path = tmp_path / 'vault.pve'
    key = b'\x31' * 32
    new_key = b'\x32' * 32
    backup = tmp_path / 'backup.pve'
    with Vault(path, key=key, create=True) as vault:
        with pytest.raises(PicoVoltError):
            Vault(path, key=key)
        vault.batch([dict(sql='CREATE TABLE t(id PRIMARY KEY,body,embedding)'),
                     dict(sql='INSERT INTO t VALUES(?,?,?)', params=[1,'private backup restore','[1,0]'])])
        before = path.read_bytes()
        with pytest.raises(PicoVoltError):
            vault.batch([dict(sql='INSERT INTO t VALUES(?,?,?)', params=[2,'rolled back','[0,1]']),
                         dict(sql='INSERT INTO t VALUES(?,?,?)', params=[1,'duplicate','[0,1]'])])
        assert path.read_bytes() == before
        assert len(vault.query('SELECT * FROM t')['rows']) == 1
        with pytest.raises(PicoVoltError):
            vault.query('DELETE FROM t')
        assert vault.retrieve(dict(kind='full_text',sql='SELECT id,body FROM t',id_column='id',text_columns=['body'],query='backup',limit=5))[0]['id']=='1'
        assert vault.backup(backup)['verified']
        vault.rotate_key(key=new_key)
        assert b'private backup restore' not in path.read_bytes()
    with pytest.raises(PicoVoltError):
        Vault(path, key=key)
    with Vault(path, key=new_key) as vault:
        assert vault.query('SELECT id FROM t')['rows'] == [[1]]
    with Vault(backup, key=key) as vault:
        assert vault.query('SELECT id FROM t')['rows'] == [[1]]
    with pytest.raises(PicoVoltError, match='closed'):
        vault.inspect()

def test_vault_rejects_bad_keys_requests_and_nul_paths(tmp_path):
    with pytest.raises(ValueError):
        Vault(tmp_path / 'v', key=b'short', create=True)
    with pytest.raises(ValueError):
        Vault('bad\0path', key=b'x'*32)
    with Vault(tmp_path / 'v', key=b'x'*32, create=True) as vault:
        with pytest.raises(PicoVoltError):
            vault._request(dict(action='unknown'))
        with pytest.raises(PicoVoltError):
            vault.batch([])
        with pytest.raises(PicoVoltError):
            vault.batch([dict(sql='SELECT 1')])
